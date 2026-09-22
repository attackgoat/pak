#![cfg(feature = "bake")]

use {
    pak::{
        Pak as _, PakBuf,
        buf::{DerivedAssetBakeCandidate, DerivedAssetBakeOutput, DerivedAssetBaker},
        mesh::{LodSet, Mesh},
    },
    std::{fs, path::Path},
};

fn fixture(root: &Path) {
    fs::copy(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/scene/cube.glb"),
        root.join("cube.glb"),
    )
    .unwrap();
    fs::write(root.join("a,\"mesh.toml"), "[mesh]\nsrc = 'cube.glb'\n").unwrap();
    fs::write(
        root.join("off.toml"),
        "[mesh]\nsrc = 'cube.glb'\ninherit-lods = false\nlods = []\n",
    )
    .unwrap();
    fs::write(
        root.join("scene.toml"),
        "[scene]\n[[scene.ref]]\nmesh = { src = 'cube.glb', offset = [2.0, 0.0, 0.0] }\n",
    )
    .unwrap();
    fs::write(root.join("pak.toml"), "[content]\ndefault-lods = [{layout='POSITION', simplify=true}, {layout='PACKED_NORMAL', simplify=true}, {layout='PACKED_NORMAL | TEXTURE0', simplify=false}]\n[content.lod]\nmin-triangles = 1\ntarget-error = 0.1\n[[content.group]]\nassets = ['a,*toml', 'off.toml', 'scene.toml']\nsegment = 'geometry'\ncompression = 'snap'\n").unwrap();
}

#[test]
fn bake_reports_cover_keyed_inline_disabled_and_exact_geometry_and_regenerate_identically() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fixture(root);
    let output = root.join("review.pak");
    let report = output.with_extension("mesh-lods.csv");
    let diagnostics = output.with_extension("mesh-lod-diagnostics.csv");
    PakBuf::bake_with_dir_without_cargo_watches(root.join("pak.toml"), &output, root).unwrap();
    let expected = fs::read(&report).unwrap();
    let expected_diagnostics = fs::read(&diagnostics).unwrap();
    let text = String::from_utf8(expected.clone()).unwrap();
    assert!(text.contains("\"a,\"\"mesh\""));
    assert!(text.contains(pak::buf::MESH_LOD_PRODUCER));
    assert!(text.contains("source-only"));
    assert!(text.contains("group-size = 4"));
    let mut pak = PakBuf::open(&output).unwrap();
    let mut expected_rows = 0;
    let scene = pak.read_scene("scene").unwrap();
    let ids = [
        pak.mesh_id("a,\"mesh").unwrap(),
        pak.mesh_id("off").unwrap(),
        scene.refs().next().unwrap().mesh().unwrap(),
    ];
    assert_eq!(pak.mesh_count(), ids.len());
    for (id, mesh_id) in ids.into_iter().enumerate() {
        let mesh = pak.read_mesh_id(mesh_id).unwrap();
        expected_rows += mesh
            .primitives()
            .iter()
            .map(|primitive| {
                1 + primitive
                    .lod_sets()
                    .iter()
                    .map(|lods| lods.levels().len())
                    .sum::<usize>()
            })
            .sum::<usize>();
        assert!(text.contains(&format!(",{id},0,")));
    }
    assert_eq!(
        text.matches(",base,source,").count()
            + text.matches(",GeometricAbsolute,").count()
            + text.matches(",AttributeWeightedAbsolute,").count()
            + text.matches(",SourceOnly,").count(),
        expected_rows
    );
    assert!(
        text.lines().any(|line| line.starts_with(",2,0,")),
        "inline-only mesh must have a numeric ID and empty key"
    );
    let archive = fs::read(&output).unwrap();
    let repeated = root.join("repeated.pak");
    PakBuf::bake_with_dir_without_cargo_watches(root.join("pak.toml"), &repeated, root).unwrap();
    assert_eq!(
        fs::read(repeated.with_extension("mesh-lods.csv")).unwrap(),
        expected
    );
    assert_eq!(
        fs::read(repeated.with_extension("mesh-lod-diagnostics.csv")).unwrap(),
        expected_diagnostics
    );
    // Simulate a cache hit with missing reports and no source assets available.
    fs::remove_file(root.join("cube.glb")).unwrap();
    fs::remove_file(&report).unwrap();
    fs::remove_file(&diagnostics).unwrap();
    pak.write_mesh_lod_report(&report).unwrap();
    pak.write_mesh_lod_diagnostics(&diagnostics).unwrap();
    assert_eq!(fs::read(&report).unwrap(), expected);
    assert_eq!(fs::read(&diagnostics).unwrap(), expected_diagnostics);
    assert_eq!(fs::read(&output).unwrap(), archive);

    // Copy the complete segmented cache artifact to another location and regenerate there.
    let cache = tempfile::tempdir().unwrap();
    for entry in fs::read_dir(root).unwrap() {
        let entry = entry.unwrap();
        if entry.file_type().unwrap().is_file() {
            fs::copy(entry.path(), cache.path().join(entry.file_name())).unwrap();
        }
    }
    let mut cached = PakBuf::open(cache.path().join("review.pak")).unwrap();
    cached
        .write_mesh_lod_report(cache.path().join("regenerated.csv"))
        .unwrap();
    cached
        .write_mesh_lod_diagnostics(cache.path().join("diagnostics.csv"))
        .unwrap();
    assert_eq!(
        fs::read(cache.path().join("regenerated.csv")).unwrap(),
        expected
    );
    assert_eq!(
        fs::read(cache.path().join("diagnostics.csv")).unwrap(),
        expected_diagnostics
    );
}

#[test]
fn report_failure_preserves_existing_archive_and_does_not_publish_new_archive() {
    for blocked_extension in ["mesh-lods.csv", "mesh-lod-diagnostics.csv"] {
        for existing in [false, true] {
            let directory = tempfile::tempdir().unwrap();
            let root = directory.path();
            fixture(root);
            let manifest = root.join("pak.toml");
            let output = root.join("review.pak");
            let blocked = output.with_extension(blocked_extension);
            let other = output.with_extension(if blocked_extension == "mesh-lods.csv" {
                "mesh-lod-diagnostics.csv"
            } else {
                "mesh-lods.csv"
            });

            if existing {
                PakBuf::bake_with_dir_without_cargo_watches(&manifest, &output, root).unwrap();
                fs::remove_file(&blocked).unwrap();
            }

            let previous_archive = fs::read(&output).ok();
            let previous_report = fs::read(&other).ok();
            fs::create_dir(&blocked).unwrap();
            fs::write(
                root.join("off.toml"),
                "[mesh]\nsrc = 'cube.glb'\ninherit-lods = false\nlods = [{layout='POSITION', simplify=false}]\n",
            )
            .unwrap();

            let error =
                PakBuf::bake_with_dir_without_cargo_watches(&manifest, &output, root).unwrap_err();
            assert!(
                format!("{error:#}").contains(blocked_extension),
                "{error:#}"
            );
            assert_eq!(fs::read(&output).ok(), previous_archive);
            assert_eq!(fs::read(&other).ok(), previous_report);
            assert!(blocked.is_dir());

            if existing {
                let mut pak = PakBuf::open(&output).unwrap();
                assert!(pak.validate_hash().unwrap());
                assert!(
                    pak.read_mesh("off").unwrap().primitives()[0]
                        .lod_sets()
                        .is_empty()
                );
            }

            fs::remove_dir(&blocked).unwrap();
            PakBuf::bake_with_dir_without_cargo_watches(&manifest, &output, root).unwrap();
            assert_ne!(fs::read(&output).ok(), previous_archive);
            let mut pak = PakBuf::open(&output).unwrap();
            assert!(pak.validate_hash().unwrap());
            assert_eq!(
                pak.read_mesh("off").unwrap().primitives()[0]
                    .lod_sets()
                    .len(),
                1
            );
            assert!(blocked.is_file());
            assert!(other.is_file());
        }
    }
}

struct RemoveProvenance;

impl DerivedAssetBaker for RemoveProvenance {
    fn bake_mesh(&mut self, mesh: &mut Mesh) -> anyhow::Result<()> {
        mesh.data = mesh
            .data
            .iter()
            .filter(|(key, _)| !key.starts_with("pak.mesh-lod."))
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .collect();
        for primitive in mesh.primitives_mut() {
            let sets = primitive
                .lod_sets()
                .iter()
                .map(|set| {
                    LodSet::new(set.metric(), set.levels().to_vec().into_boxed_slice()).unwrap()
                })
                .collect();
            primitive.set_lods(sets)?;
        }
        Ok(())
    }

    fn bake(
        &mut self,
        _: &DerivedAssetBakeCandidate<'_>,
    ) -> anyhow::Result<Box<[DerivedAssetBakeOutput]>> {
        Ok(Box::new([]))
    }
}

#[test]
fn archive_without_provenance_never_guesses_settings_disabled_or_stop_reasons() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fixture(root);
    let output = root.join("unknown.pak");
    PakBuf::bake_with_dir_and_derived_assets_without_cargo_watches(
        root.join("pak.toml"),
        &output,
        root,
        &mut RemoveProvenance,
    )
    .unwrap();
    let text = fs::read_to_string(output.with_extension("mesh-lods.csv")).unwrap();
    assert!(!text.contains(pak::buf::MESH_LOD_PRODUCER));
    assert!(!text.contains("triangle-floor"));
    assert!(!text.contains("stalled-after-regrouping"));
    assert!(!text.contains("target-error ="));
    assert!(!text.contains(",disabled,"));
    for row in text.lines().skip(1) {
        assert!(
            row.ends_with(",,,,"),
            "provenance columns must be empty: {row}"
        );
    }
    assert!(text.contains(",full-detail,,,,"));
}

#[test]
fn duplicate_default_lod_layouts_fail_without_selected_meshes() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("pak.toml");
    fs::write(
        &manifest,
        "[content]\ndefault-lods = [{layout='POSITION', simplify=true}, {layout='POSITION', simplify=false}]",
    )
    .unwrap();
    let output = directory.path().join("invalid.pak");
    let error = PakBuf::bake(&manifest, &output).unwrap_err();
    assert!(format!("{error:#}").contains("duplicate default lod vertex layout request"));
    assert!(!output.exists());
}

#[test]
fn invalid_pack_settings_fail_even_without_selected_meshes() {
    let directory = tempfile::tempdir().unwrap();
    let manifest = directory.path().join("pak.toml");
    fs::write(&manifest, "[content.lod]\ncluster-triangles = 7").unwrap();
    let output = directory.path().join("invalid.pak");
    assert!(PakBuf::bake(&manifest, &output).is_err());
    assert!(!output.exists());
}

#[test]
fn empty_mesh_report_records_unsupported_primitive_outcome() {
    let directory = tempfile::tempdir().unwrap();
    let root = directory.path();
    fs::write(root.join("point.bin"), [0_u8; 12]).unwrap();
    fs::write(root.join("point.gltf"), r#"{
        "asset": {"version": "2.0"},
        "buffers": [{"uri": "point.bin", "byteLength": 12}],
        "bufferViews": [{"buffer": 0, "byteLength": 12}],
        "accessors": [{"bufferView": 0, "componentType": 5126, "count": 1, "type": "VEC3", "min": [0,0,0], "max": [0,0,0]}],
        "meshes": [{"primitives": [{"attributes": {"POSITION": 0}, "mode": 0}]}],
        "nodes": [{"mesh": 0}], "scenes": [{"nodes": [0]}], "scene": 0
    }"#).unwrap();
    fs::write(
        root.join("pak.toml"),
        "[[content.group]]\nassets = ['point.gltf']",
    )
    .unwrap();
    let output = root.join("empty.pak");
    PakBuf::bake(root.join("pak.toml"), &output).unwrap();
    let text = fs::read_to_string(output.with_extension("mesh-lods.csv")).unwrap();
    assert!(text.contains("point.gltf,0,,,base,source,"));
    assert!(text.contains("no-supported-triangle-primitives"));
}
