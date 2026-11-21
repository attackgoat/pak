#![cfg(feature = "bake")]

use {
    pak::{Pak as _, PakBuf},
    serde_json::json,
    std::fs,
};

#[test]
fn shared_geometry_preserves_mesh_selections_transforms_ids_and_archive_bytes() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let positions = [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]];
    let geometry = positions
        .into_iter()
        .flatten()
        .flat_map(f32::to_le_bytes)
        .collect::<Vec<_>>();
    fs::write(root.join("geometry.bin"), &geometry).unwrap();
    let mut document = json!({
        "asset": {"version": "2.0"},
        "buffers": [{"uri": "geometry.bin", "byteLength": geometry.len()}],
        "bufferViews": [{"buffer": 0, "byteLength": geometry.len()}],
        "accessors": [{
            "bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3",
            "min": [0, 0, 0], "max": [1, 1, 0],
        }],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}}]}],
        "nodes": [
            {"name": "first", "mesh": 0, "translation": [2, 0, 0]},
            {"name": "second", "mesh": 0, "translation": [4, 0, 0]},
            {"name": "alternate", "mesh": 0, "translation": [8, 0, 0]},
        ],
        "scenes": [{"nodes": [0, 1]}, {"name": "alternate", "nodes": [2]}],
        "scene": 0,
    });
    let source = root.join("source.gltf");
    fs::write(&source, serde_json::to_vec(&document).unwrap()).unwrap();
    for (name, scene) in [
        ("first", ""),
        ("second", ""),
        ("alternate", "scene-name = 'alternate'\n"),
    ] {
        fs::write(
            root.join(format!("{name}.toml")),
            format!(
                "[mesh]\nsrc = 'source.gltf'\nname = '{name}'\n{scene}normals = false\ntangents = false\noptimize = false\n"
            ),
        )
        .unwrap();
    }
    let manifest = root.join("pak.toml");
    fs::write(
        &manifest,
        "[[content.group]]\nassets = ['first.toml', 'second.toml', 'alternate.toml']\n",
    )
    .unwrap();
    let original = root.join("original.pak");
    PakBuf::bake_with_dir_without_cargo_watches(&manifest, &original, root).unwrap();

    // Mesh imports must not read image pixels, even when another recipe selects this source.
    document["images"] = json!([{ "uri": "missing.png" }]);
    fs::write(&source, serde_json::to_vec(&document).unwrap()).unwrap();
    let output = root.join("images.pak");
    PakBuf::bake_with_dir_without_cargo_watches(&manifest, &output, root).unwrap();
    assert_eq!(fs::read(original).unwrap(), fs::read(&output).unwrap());

    let mut pak = PakBuf::open(output).unwrap();
    for (name, offset) in [("first", 2.0), ("second", 4.0), ("alternate", 8.0)] {
        let mesh = pak.read_mesh(name).unwrap();
        let base = mesh.primitives()[0].base();
        assert_eq!(base.indices().as_u32(), [0, 1, 2]);
        let actual = base
            .vertex_data()
            .chunks_exact(base.vertex_type().stride())
            .map(|vertex| {
                std::array::from_fn::<_, 3, _>(|axis| {
                    f32::from_ne_bytes(vertex[axis * 4..axis * 4 + 4].try_into().unwrap())
                })
            })
            .collect::<Vec<_>>();
        assert_eq!(actual, positions.map(|[x, y, z]| [x + offset, y, z]));
    }
}
