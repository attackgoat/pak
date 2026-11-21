#![cfg(feature = "bake")]

use {
    pak::{
        Pak as _, PakBuf,
        mesh::{Geometry, LodMetric, Mesh, VertexType},
    },
    std::{collections::BTreeSet, fs, path::Path},
};

fn fixture(root: &Path, deformed: bool) {
    let mut bytes = Vec::new();
    for y in 0..17 {
        for x in 0..17 {
            for value in [
                x as f32,
                y as f32,
                0.0,
                0.0,
                0.0,
                1.0,
                x as f32 / 16.0,
                y as f32 / 16.0,
                y as f32 / 8.0,
                x as f32 / 4.0,
                1.0,
                0.0,
                0.0,
                -1.0,
            ] {
                bytes.extend(value.to_le_bytes());
            }
        }
    }
    let vertex_bytes = bytes.len();
    let mut counts = [0; 2];
    for (member, count) in counts.iter_mut().enumerate() {
        for y in 0..16 {
            for x in 0..16 {
                if usize::from(x >= 8) != member {
                    continue;
                }
                let i = y * 17 + x;
                for index in [i, i + 1, i + 17, i + 1, i + 18, i + 17] {
                    bytes.extend((index as u32).to_le_bytes());
                    *count += 1;
                }
            }
        }
    }
    fs::write(root.join("grid.bin"), &bytes).unwrap();
    let byte_count = bytes.len();
    let offset1 = vertex_bytes + counts[0] * 4;
    let [count0, count1] = counts;
    let length0 = count0 * 4;
    let length1 = count1 * 4;
    let targets = if deformed {
        r#", "targets":[{"POSITION":0}]"#
    } else {
        ""
    };
    fs::write(root.join("grid.gltf"), format!(r#"{{
      "asset":{{"version":"2.0"}},
      "buffers":[{{"uri":"grid.bin","byteLength":{byte_count}}}],
      "bufferViews":[
        {{"buffer":0,"byteLength":{vertex_bytes},"byteStride":56}},
        {{"buffer":0,"byteOffset":{vertex_bytes},"byteLength":{length0}}},
        {{"buffer":0,"byteOffset":{offset1},"byteLength":{length1}}}
      ],
      "accessors":[
        {{"bufferView":0,"componentType":5126,"count":289,"type":"VEC3","min":[0,0,0],"max":[16,16,0]}},
        {{"bufferView":0,"byteOffset":12,"componentType":5126,"count":289,"type":"VEC3"}},
        {{"bufferView":0,"byteOffset":24,"componentType":5126,"count":289,"type":"VEC2"}},
        {{"bufferView":0,"byteOffset":32,"componentType":5126,"count":289,"type":"VEC2"}},
        {{"bufferView":0,"byteOffset":40,"componentType":5126,"count":289,"type":"VEC4"}},
        {{"bufferView":1,"componentType":5125,"count":{count0},"type":"SCALAR"}},
        {{"bufferView":2,"componentType":5125,"count":{count1},"type":"SCALAR"}}
      ],
      "materials":[{{}},{{"alphaMode":"BLEND"}}],
      "meshes":[{{"primitives":[
        {{"attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2,"TEXCOORD_1":3,"TANGENT":4}},"indices":5,"material":1{targets}}},
        {{"attributes":{{"POSITION":0,"NORMAL":1,"TEXCOORD_0":2,"TEXCOORD_1":3,"TANGENT":4}},"indices":6,"material":0{targets}}}
      ]}}],
      "nodes":[{{"mesh":0}}],"scenes":[{{"nodes":[0]}}],"scene":0
    }}"#)).unwrap();
    for (name, options) in [
        ("inherit", ""),
        ("off", "source-lods=false\n"),
        ("no-inherit", "inherit-lods=false\n"),
        (
            "local",
            "source-lods=true\n[mesh.lod]\ntexture-weight=0.25\n",
        ),
        ("local-no-inherit", "inherit-lods=false\nsource-lods=true\n"),
        (
            "exact",
            "source-lods=true\nlods=[{layout='NORMAL | TEXTURE0 | TEXTURE1 | TANGENT',simplify=false}]\n",
        ),
    ] {
        fs::write(
            root.join(format!("{name}.toml")),
            format!("[mesh]\nsrc='grid.gltf'\noptimize=false\n{options}"),
        )
        .unwrap();
    }
    fs::write(
        root.join("scene.toml"),
        "[scene]\n[[scene.ref]]\nmesh={src='grid.gltf',optimize=false,offset=[40.0,0.0,0.0]}\n",
    )
    .unwrap();
}

fn bake(root: &Path, defaults: bool, grouped: bool) -> PakBuf {
    let requests = if grouped {
        "default-lods=[{layout='PACKED_NORMAL',simplify=true}]\n"
    } else {
        ""
    };
    fs::write(root.join("pak.toml"), format!(
        "[content]\ndefault-source-lods={defaults}\n{requests}[content.lod]\njoin={grouped}\nmin-triangles=8\ntarget-error=0.1\nlock-border=true\n[[content.group]]\nassets=['grid.gltf','inherit.toml','off.toml','no-inherit.toml','local.toml','local-no-inherit.toml','exact.toml','scene.toml']\n"
    )).unwrap();
    let output = root.join("test.pak");
    PakBuf::bake_with_dir_without_cargo_watches(root.join("pak.toml"), &output, root).unwrap();
    PakBuf::open(output).unwrap()
}

fn assert_source_chain(mesh: &Mesh, reduced: bool) {
    assert_eq!(mesh.primitives().len(), 2);
    for (idx, primitive) in mesh.primitives().iter().enumerate() {
        assert_eq!(primitive.material(), (1 - idx) as u8);
        let source = primitive.base();
        let layout =
            VertexType::NORMAL | VertexType::TEXTURE0 | VertexType::TEXTURE1 | VertexType::TANGENT;
        assert_eq!(source.vertex_type(), layout);
        let set = primitive.lod_set(layout).unwrap();
        let first = set.levels()[0].geometry();
        assert_eq!(first.vertex_type(), source.vertex_type());
        assert_eq!(first.vertex_data(), source.vertex_data());
        assert_eq!(oriented_triangles(first), oriented_triangles(source));
        assert_eq!(set.levels().len() > 1, reduced);
        assert_eq!(
            set.provenance().unwrap().producer,
            pak::buf::MESH_LOD_PRODUCER
        );
        let records = referenced_records(source);
        for level in &set.levels()[1..] {
            assert_eq!(level.geometry().vertex_type(), layout);
            assert!(
                level.geometry().indices().triangle_count() < source.indices().triangle_count()
            );
            assert!(
                level
                    .geometry()
                    .vertex_data()
                    .chunks_exact(layout.stride())
                    .all(|record| records.contains(record))
            );
            let mut referenced = level.geometry().indices().as_u32();
            referenced.sort_unstable();
            referenced.dedup();
            assert_eq!(
                referenced,
                (0..level.geometry().vertex_count() as u32).collect::<Vec<_>>()
            );
        }
    }
}

#[test]
fn source_lods_resolve_pack_defaults_local_overrides_and_inline_meshes() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, false);
    let mut disabled = bake(root, false, false);
    let baseline = disabled.read_mesh("inherit").unwrap();
    assert!(
        baseline
            .primitives()
            .iter()
            .all(|primitive| primitive.lod_sets().is_empty())
    );
    for key in ["local", "local-no-inherit", "exact"] {
        assert_source_chain(&disabled.read_mesh(key).unwrap(), key != "exact");
    }
    let mut enabled = bake(root, true, false);
    for key in ["inherit", "local"] {
        let mesh = enabled.read_mesh(key).unwrap();
        assert_source_chain(&mesh, true);
        for (base, primitive) in baseline.primitives().iter().zip(mesh.primitives()) {
            assert_eq!(base.base(), primitive.base());
            assert_eq!(base.material(), primitive.material());
        }
    }
    for key in ["off", "no-inherit"] {
        assert!(
            enabled
                .read_mesh(key)
                .unwrap()
                .primitives()
                .iter()
                .all(|primitive| primitive.lod_sets().is_empty())
        );
    }
    let exact = enabled.read_mesh("exact").unwrap();
    assert_source_chain(&exact, false);
    assert!(
        exact
            .primitives()
            .iter()
            .all(|primitive| primitive.lod_sets()[0].metric() == LodMetric::SourceOnly)
    );
    let direct = enabled.read_mesh("grid.gltf").unwrap();
    assert_source_chain(&direct, true);
    let scene = enabled.read_scene("scene").unwrap();
    let inline = enabled
        .read_mesh_id(scene.refs().next().unwrap().mesh().unwrap())
        .unwrap();
    assert_source_chain(&inline, true);
    let local = enabled.read_mesh("local").unwrap();
    assert!(
        local.primitives()[0].lod_sets()[0]
            .provenance()
            .unwrap()
            .settings
            .contains("texture-weight = 0.25")
    );
}

fn oriented_triangles(geometry: &Geometry) -> Vec<[u32; 3]> {
    let mut triangles = geometry
        .indices()
        .as_u32()
        .chunks_exact(3)
        .map(|face| {
            let [a, b, c] = <[u32; 3]>::try_from(face).unwrap();
            [a, b, c].min([b, c, a]).min([c, a, b])
        })
        .collect::<Vec<_>>();
    triangles.sort_unstable();
    triangles
}

fn referenced_records(geometry: &Geometry) -> BTreeSet<&[u8]> {
    let indices = geometry.indices().as_u32();
    indices
        .into_iter()
        .map(|idx| {
            let offset = idx as usize * geometry.vertex_type().stride();
            &geometry.vertex_data()[offset..offset + geometry.vertex_type().stride()]
        })
        .collect()
}

fn seam_records(geometry: &Geometry) -> BTreeSet<&[u8]> {
    referenced_records(geometry)
        .into_iter()
        .filter(|record| f32::from_ne_bytes(record[..4].try_into().unwrap()) == 8.0)
        .collect()
}

#[test]
fn primitive_source_chains_coexist_with_groups_and_protect_material_seams() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, false);
    let mesh = bake(root, true, true).read_mesh("inherit").unwrap();
    assert_eq!(mesh.lod_groups().len(), 1);
    mesh.lod_groups()[0]
        .validate_source(mesh.primitives())
        .unwrap();
    assert_source_chain(&mesh, true);
    for primitive in mesh.primitives() {
        let source = primitive.base();
        let canonical_seam = seam_records(source);
        assert_eq!(canonical_seam.len(), 17);
        for level in primitive.lod_set(source.vertex_type()).unwrap().levels() {
            assert_eq!(seam_records(level.geometry()), canonical_seam);
        }
    }
    // Choosing the other member's canonical geometry does not remove or redistribute
    // this primitive's triangles; grouped snapshots are not involved in that choice.
    assert_eq!(mesh.primitives()[0].material(), 1);
    assert_eq!(mesh.primitives()[1].material(), 0);
}

#[test]
fn automatic_source_requests_respect_group_layout_ownership() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, false);
    let layout = "NORMAL | TEXTURE0 | TEXTURE1 | TANGENT";
    fs::write(
        root.join("local.toml"),
        format!(
            "[mesh]\nsrc='grid.gltf'\nsource-lods=true\ninherit-lods=false\nlods=[{{layout='{layout}',simplify=true}}]\n"
        ),
    )
    .unwrap();
    for (processing, group_count) in [("join=true", 1), ("voxelize=true\nvoxel-size=1.0", 2)] {
        fs::write(
            root.join("pak.toml"),
            format!(
                "[content]\ndefault-source-lods=true\ndefault-lods=[{{layout='{layout}',simplify=true}}]\n[content.lod]\n{processing}\nmin-triangles=8\ntarget-error=0.1\n[[content.group]]\nassets=['inherit.toml','local.toml']\n"
            ),
        )
        .unwrap();
        let output = root.join("grouped.pak");
        PakBuf::bake_with_dir_without_cargo_watches(root.join("pak.toml"), &output, root).unwrap();
        let mut pak = PakBuf::open(output).unwrap();
        for key in ["inherit", "local"] {
            let mesh = pak.read_mesh(key).unwrap();
            assert_eq!(mesh.lod_groups().len(), group_count);
            for group in mesh.lod_groups() {
                group.validate_source(mesh.primitives()).unwrap();
            }
            for (index, primitive) in mesh.primitives().iter().enumerate() {
                let layout = primitive.base().vertex_type();
                assert!(primitive.lod_set(layout).is_none());
                assert!(mesh.lod_groups().iter().any(|group| {
                    group.vertex_type() == layout
                        && group
                            .members()
                            .iter()
                            .any(|member| member.primitive().index() == index)
                }));
            }
        }
    }
}

#[test]
fn automatic_source_requests_skip_deformation_even_when_imported_attributes_are_rigid() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root, true);
    // Explicit layout requests have their own contract; test only automatic requests.
    for name in ["local", "exact"] {
        fs::write(
            root.join(format!("{name}.toml")),
            "[mesh]\nsrc='grid.gltf'\nsource-lods=true\n",
        )
        .unwrap();
    }
    let mut pak = bake(root, true, false);
    for key in ["grid.gltf", "inherit", "local", "exact"] {
        let mesh = pak.read_mesh(key).unwrap();
        assert!(
            mesh.primitives()
                .iter()
                .all(|primitive| primitive.lod_sets().is_empty())
        );
        assert!(
            mesh.data("pak.source-deformation")
                .unwrap()
                .as_iter()
                .unwrap()
                .all(|value| value.as_bool() == Some(true))
        );
    }
}
