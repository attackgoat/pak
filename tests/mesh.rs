#![cfg(feature = "bake")]

use {
    pak::{
        Pak as _, PakBuf,
        buf::{DerivedAssetBakeCandidate, DerivedAssetBakeOutput, DerivedAssetBaker},
        mesh::{Mesh, VertexType},
        scene::DataData,
    },
    std::{fs, io::ErrorKind, path::Path},
};

struct MetadataBaker {
    calls: usize,
    fail: bool,
}

impl DerivedAssetBaker for MetadataBaker {
    fn bake_mesh(&mut self, mesh: &mut Mesh) -> anyhow::Result<()> {
        self.calls += 1;
        anyhow::ensure!(!self.fail, "mesh hook failed");
        assert!(mesh.blob().is_some());
        assert!(mesh.data("authored").is_some());
        assert!(!mesh.primitives().is_empty());
        for primitive in mesh.primitives() {
            let base = primitive.base();
            assert!(base.indices().triangle_count() > 0);
            for vertex in base.vertex_data().chunks_exact(base.vertex_type().stride()) {
                let x = f32::from_ne_bytes(vertex[..4].try_into().unwrap());
                assert!(x > 90.0, "hook must observe the baked translation");
            }
        }
        mesh.data
            .insert("generated", DataData::Array(vec![DataData::Float(1.25)]));
        Ok(())
    }

    fn bake(
        &mut self,
        _: &DerivedAssetBakeCandidate<'_>,
    ) -> anyhow::Result<Box<[DerivedAssetBakeOutput]>> {
        panic!("standalone meshes must not produce OMM candidates")
    }
}

fn fixture(root: &Path) {
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/scene/cube.glb"),
        root.join("cube.glb"),
    )
    .unwrap();
    fs::write(root.join("payload.bin"), b"mesh payload").unwrap();
    for (name, data) in [
        (
            "a",
            "authored = ['label', [true, -7, 0.25]]\ngenerated = 'replace me'",
        ),
        (
            "b",
            "generated = 'replace me'\nauthored = ['label', [true, -7, 0.25]]",
        ),
        ("c", "authored = ['different']\ngenerated = 'replace me'"),
    ] {
        fs::write(root.join(format!("{name}.toml")), format!(
            "[mesh]\nsrc = 'cube.glb'\nblob = 'payload.bin'\noffset = [100.0, 0.0, 0.0]\nlods = [{{layout='POSITION', simplify=true}}, {{layout='PACKED_NORMAL', simplify=true}}]\n[mesh.data]\n{data}\n"
        )).unwrap();
    }
    fs::write(root.join("pak.toml"),
        "[[content.group]]\nname = 'meshes'\nsegment = 'geometry'\ncompression = 'snap'\nassets = ['a.toml', 'b.toml', 'c.toml']\n"
    ).unwrap();
}

#[test]
fn mesh_metadata_and_blob_round_trip_with_final_hook_and_identity() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let output = root.join("meshes.pak");
    let mut baker = MetadataBaker {
        calls: 0,
        fail: false,
    };
    PakBuf::bake_with_dir_and_derived_assets_without_cargo_watches(
        root.join("pak.toml"),
        &output,
        root,
        &mut baker,
    )
    .unwrap();
    assert_eq!(baker.calls, 2, "key ordering must not change mesh identity");
    let mut pak = PakBuf::open(&output).unwrap();
    let a = pak.read_mesh("a").unwrap();
    let b = pak.read_mesh("b").unwrap();
    let c = pak.read_mesh("c").unwrap();
    assert_eq!(a.data, b.data);
    assert_ne!(a.data, c.data);
    let authored = a
        .data("authored")
        .unwrap()
        .expect_iter()
        .collect::<Vec<_>>();
    assert_eq!(authored[0].expect_str(), "label");
    let nested = authored[1].expect_iter().collect::<Vec<_>>();
    assert!(nested[0].expect_bool());
    assert_eq!(nested[1].expect_i32(), -7);
    assert_eq!(nested[2].expect_f32(), 0.25);
    assert_eq!(
        a.data("generated")
            .unwrap()
            .expect_iter()
            .next()
            .unwrap()
            .expect_f32(),
        1.25
    );
    assert!(a.data("missing").is_none());
    assert_eq!(
        pak.read_blob_id(a.blob().unwrap()).unwrap(),
        b"mesh payload"
    );
    assert!(pak.opacity_micromap_infos().is_empty());
}

#[test]
fn mesh_metadata_without_hook_preserves_authored_values() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let output = root.join("meshes.pak");
    PakBuf::bake_with_dir_without_cargo_watches(root.join("pak.toml"), &output, root).unwrap();
    let mut pak = PakBuf::open(&output).unwrap();
    let mesh = pak.read_mesh("a").unwrap();
    assert_eq!(mesh.data("generated").unwrap().expect_str(), "replace me");
    assert_eq!(
        pak.read_blob_id(mesh.blob().unwrap()).unwrap(),
        b"mesh payload"
    );
}

#[test]
fn mesh_hook_failure_does_not_publish_archive() {
    let dir = tempfile::tempdir().unwrap();
    let root = dir.path();
    fixture(root);
    let output = root.join("meshes.pak");
    let mut baker = MetadataBaker {
        calls: 0,
        fail: true,
    };
    let error = PakBuf::bake_with_dir_and_derived_assets_without_cargo_watches(
        root.join("pak.toml"),
        &output,
        root,
        &mut baker,
    )
    .unwrap_err();
    assert!(format!("{error:#}").contains("mesh hook failed"));
    assert!(error.to_string().contains("baking mesh metadata for mesh"));
    assert!(!output.exists());
}

#[test]
fn authored_tangents_without_uv0_bake_and_round_trip() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    let vertices = [[0.0_f32, 0.0, 0.0], [1.0, 0.0, 0.0], [0.0, 1.0, 0.0]]
        .into_iter()
        .flat_map(|position| {
            position
                .into_iter()
                .chain([0.0, 0.0, 1.0, 0.0, 1.0, 0.0, -1.0])
        })
        .collect::<Vec<_>>();
    fs::write(
        root.join("tangents.bin"),
        vertices
            .iter()
            .flat_map(|value| value.to_le_bytes())
            .collect::<Vec<_>>(),
    )
    .unwrap();
    fs::write(root.join("tangents.gltf"), r#"{
        "asset": {"version": "2.0"},
        "buffers": [{"uri": "tangents.bin", "byteLength": 120}],
        "bufferViews": [{"buffer": 0, "byteLength": 120, "byteStride": 40}],
        "accessors": [
            {"bufferView": 0, "componentType": 5126, "count": 3, "type": "VEC3", "min": [0,0,0], "max": [1,1,0]},
            {"bufferView": 0, "byteOffset": 12, "componentType": 5126, "count": 3, "type": "VEC3"},
            {"bufferView": 0, "byteOffset": 24, "componentType": 5126, "count": 3, "type": "VEC4"}
        ],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TANGENT": 2}}]}],
        "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0
    }"#).unwrap();
    fs::write(
        root.join("mesh.toml"),
        "[mesh]\nsrc = 'tangents.gltf'\noptimize = false\n",
    )
    .unwrap();
    let expected = vertices
        .iter()
        .flat_map(|value| value.to_ne_bytes())
        .collect::<Vec<_>>();
    for requests in ["[]", "[{layout = 'NORMAL | TANGENT', simplify = false}]"] {
        fs::write(
            root.join("pak.toml"),
            format!(
                "[content]\ndefault-lods = {requests}\n[[content.group]]\nassets = ['mesh.toml']\n"
            ),
        )
        .unwrap();
        let output = root.join("tangents.pak");
        PakBuf::bake_with_dir_without_cargo_watches(root.join("pak.toml"), &output, root).unwrap();
        let mut pak = PakBuf::open(output).unwrap();
        let mesh = pak.read_mesh("mesh").unwrap();
        let primitive = &mesh.primitives()[0];
        let base = primitive.base();
        assert_eq!(base.vertex_type(), VertexType::NORMAL | VertexType::TANGENT);
        assert_eq!(base.vertex_data(), expected);
        assert_eq!(base.indices().as_u32(), [0, 1, 2]);
        assert!(base.texture0().is_none());

        if requests != "[]" {
            let set = primitive.lod_set(base.vertex_type()).unwrap();
            assert_eq!(set.levels().len(), 1);
            assert_eq!(set.levels()[0].geometry().vertex_data(), expected);
        }
    }
}

#[test]
fn previous_mesh_format_is_rejected_before_header_decode() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("old.pak");
    fs::write(&output, b"ATTACKGOAT-PAK-V1.9 ").unwrap();
    let error = PakBuf::open(output).err().unwrap();
    assert_eq!(error.kind(), ErrorKind::InvalidData);
    assert!(error.to_string().contains("V1.10"));
    assert!(error.to_string().contains("rebake"));
}

#[test]
fn pack_lod_defaults_cover_direct_keyed_and_inline_imports_with_explicit_overrides() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/scene/cube.glb"),
        root.join("cube.glb"),
    )
    .unwrap();
    for (name, flags) in [
        ("z-inherit", ""),
        ("z-off", "inherit-lods = false"),
        ("z-empty", "lods = []"),
        ("z-override", "lods = [{layout='POSITION', simplify=false}]"),
        (
            "z-on",
            "lods = [{layout='POSITION', simplify=true}, {layout='PACKED_NORMAL', simplify=true}]\nmin-lod-triangles = 7\n[mesh.lod]\nmin-triangles = 9\ngroup-size = 2",
        ),
        (
            "z-position-only",
            "inherit-lods = false\nlods = [{layout='POSITION', simplify=true}]",
        ),
        (
            "z-normal-only",
            "inherit-lods = false\nlods = [{layout='PACKED_NORMAL', simplify=true}]",
        ),
        ("dependency", "offset = [2.0, 0.0, 0.0]"),
    ] {
        fs::write(
            root.join(format!("{name}.toml")),
            format!("[mesh]\nsrc = 'cube.glb'\n{flags}\n"),
        )
        .unwrap();
    }
    fs::write(
        root.join("a-scene.toml"),
        r#"
        [scene]
        [[scene.ref]]
        id = 'direct'
        mesh = 'cube.glb'
        [[scene.ref]]
        id = 'keyed'
        mesh = 'z-inherit.toml'
        [[scene.ref]]
        id = 'inline'
        mesh = { src = 'cube.glb', offset = [1.0, 0.0, 0.0], lod = { alignment-weight = 2.0 } }
        [[scene.ref]]
        id = 'inline-off'
        mesh = { src = 'cube.glb', inherit-lods = false, lods = [] }
        [[scene.ref]]
        id = 'inline-empty'
        mesh = { src = 'cube.glb', lods = [] }
        [[scene.ref]]
        id = 'inline-override'
        mesh = { src = 'cube.glb', lods = [{layout='POSITION', simplify=false}] }
        [[scene.ref]]
        id = 'inline-on'
        mesh = { src = 'cube.glb', lods = [{layout='POSITION', simplify=true}, {layout='PACKED_NORMAL', simplify=true}] }
        [[scene.ref]]
        id = 'dependency'
        mesh = 'dependency.toml'
    "#,
    )
    .unwrap();
    let mut previous_ids = None;
    for defaults in [
        Some((true, true)),
        None,
        Some((false, false)),
        Some((true, false)),
        Some((false, true)),
    ] {
        let fields = defaults
            .map(|(normal, position)| {
                format!(
                    "default-lods = [{}]",
                    [
                        normal.then_some("{layout='PACKED_NORMAL', simplify=true}"),
                        position.then_some("{layout='POSITION', simplify=true}")
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(",")
                )
            })
            .unwrap_or_default();
        let manifest = root.join("pak.toml");
        fs::write(&manifest, format!(
            "[content]\n{fields}\n[content.lod]\nmin-triangles = 23\ngroup-size = 8\nnormal-weight = 0.25\n[[content.group]]\nsegment = 'scene'\ncompression = 'snap'\nassets = ['a-scene.toml']\n\n[[content.group]]\nsegment = 'meshes'\ncompression = 'none'\nassets = ['cube.glb', 'z-*.toml']\n"
        )).unwrap();
        let output = root.join("meshes.pak");
        PakBuf::bake_with_dir_without_cargo_watches(&manifest, &output, root).unwrap();
        let mut pak = PakBuf::open(output).unwrap();
        let (normal, position) = defaults.unwrap_or_default();
        let assert_lods = |mesh: Mesh, normal, position| {
            let settings: toml::Table =
                toml::from_str(mesh.data("pak.mesh-lod.settings").unwrap().expect_str()).unwrap();
            assert_eq!(settings["normal-weight"].as_float(), Some(0.25));
            assert!(!mesh.primitives().is_empty());
            for primitive in mesh.primitives() {
                assert_eq!(
                    primitive.lod_set(VertexType::PACKED_NORMAL).is_some(),
                    normal
                );
                assert_eq!(primitive.lod_set(VertexType::POSITION).is_some(), position);
            }
        };
        let mut ids = Vec::new();
        for (key, normal, position) in [
            ("cube.glb", normal, position),
            ("z-inherit", normal, position),
            ("z-empty", normal, position),
            ("z-override", normal, true),
            ("z-off", false, false),
            ("z-on", true, true),
            ("z-position-only", false, true),
            ("z-normal-only", true, false),
            ("dependency", normal, position),
        ] {
            ids.push(pak.mesh_id(key).unwrap());
            assert_lods(pak.read_mesh(key).unwrap(), normal, position);
        }
        assert_eq!(pak.mesh_id("cube.glb"), pak.mesh_id("z-inherit"));
        assert_ne!(pak.mesh_id("z-inherit"), pak.mesh_id("z-on"));
        assert_ne!(pak.mesh_id("z-inherit"), pak.mesh_id("z-off"));
        let overridden = pak.read_mesh("z-override").unwrap();
        for primitive in overridden.primitives() {
            let set = primitive.lod_set(VertexType::POSITION).unwrap();
            assert_eq!(set.metric(), pak::mesh::LodMetric::SourceOnly);
            assert_eq!(set.levels().len(), 1);
        }
        let on = pak.read_mesh("z-on").unwrap();
        let settings: toml::Table =
            toml::from_str(on.data("pak.mesh-lod.settings").unwrap().expect_str()).unwrap();
        assert_eq!(settings["min-triangles"].as_integer(), Some(7));
        assert_eq!(settings["group-size"].as_integer(), Some(2));
        let direct = pak.read_mesh("cube.glb").unwrap();
        let settings: toml::Table =
            toml::from_str(direct.data("pak.mesh-lod.settings").unwrap().expect_str()).unwrap();
        assert_eq!(settings["min-triangles"].as_integer(), Some(23));
        assert_eq!(settings["group-size"].as_integer(), Some(8));
        let scene = pak.read_scene("a-scene").unwrap();
        for reference in scene.refs() {
            let id = reference.mesh().unwrap();
            if reference.id() == Some("inline") {
                let mesh = pak.read_mesh_id(id).unwrap();
                let settings: toml::Table =
                    toml::from_str(mesh.data("pak.mesh-lod.settings").unwrap().expect_str())
                        .unwrap();
                assert_eq!(settings["alignment-weight"].as_float(), Some(2.0));
                assert_eq!(settings["group-size"].as_integer(), Some(8));
            }
            let expected = match reference.id().unwrap() {
                "inline-off" => (false, false),
                "inline-on" => (true, true),
                "inline-override" => (normal, true),
                _ => (normal, position),
            };
            assert_lods(pak.read_mesh_id(id).unwrap(), expected.0, expected.1);
            if reference.id() == Some("inline-override") {
                let mesh = pak.read_mesh_id(id).unwrap();
                for primitive in mesh.primitives() {
                    assert_eq!(
                        primitive.lod_set(VertexType::POSITION).unwrap().metric(),
                        pak::mesh::LodMetric::SourceOnly
                    );
                }
            }
            ids.push(id);
        }
        if let Some(previous_ids) = previous_ids {
            assert_eq!(
                ids, previous_ids,
                "pack defaults must not change raw mesh identities"
            );
        }
        previous_ids = Some(ids);
    }
}
