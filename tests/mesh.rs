#![cfg(feature = "bake")]

use {
    pak::{
        Pak as _, PakBuf,
        buf::{DerivedAssetBakeCandidate, DerivedAssetBakeOutput, DerivedAssetBaker},
        mesh::Mesh,
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
            assert!(!primitive.lods().is_empty());
            for vertex in primitive
                .vertex_data()
                .chunks_exact(primitive.vertex_type().stride())
            {
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
            "[mesh]\nsrc = 'cube.glb'\nblob = 'payload.bin'\noffset = [100.0, 0.0, 0.0]\nshadow = true\n[mesh.data]\n{data}\n"
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
fn previous_mesh_format_is_rejected_before_header_decode() {
    let dir = tempfile::tempdir().unwrap();
    let output = dir.path().join("old.pak");
    fs::write(&output, b"ATTACKGOAT-PAK-V1.7 ").unwrap();
    assert_eq!(
        PakBuf::open(output).err().unwrap().kind(),
        ErrorKind::InvalidData
    );
}
