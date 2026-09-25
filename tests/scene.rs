use {
    glam::{EulerRot, Quat},
    pak::{MaterialParameterFlags, Pak, PakBuf},
    std::{io::Error, path::PathBuf, sync::LazyLock},
};

#[cfg(feature = "bake")]
use std::{fs, io::Read};

const EPSILON: f32 = 0.0001;

static CARGO_MANIFEST_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
static TESTS_DATA_DIR: LazyLock<PathBuf> = LazyLock::new(|| CARGO_MANIFEST_DIR.join("tests/data"));

#[test]
fn deserialize_scene_materials() -> Result<(), Error> {
    let pak_dst = TESTS_DATA_DIR.join("scene/test.pak");

    #[cfg(feature = "bake")]
    {
        let pak_src = TESTS_DATA_DIR.join("scene/pak.toml");
        PakBuf::bake(&pak_src, &pak_dst).unwrap();
        fs::remove_file(pak_dst.with_extension("mesh-lods.csv"))?;
        fs::remove_file(pak_dst.with_extension("mesh-lod-diagnostics.csv"))?;
    }

    let mut pak = PakBuf::open(&pak_dst)?;
    let scene_01 = pak.read_scene("scene")?;
    let find_ref = |id| scene_01.refs().find(|r| r.id() == Some(id));

    {
        let mesh_ref = find_ref("mesh-with-one-material").unwrap();

        assert_eq!(mesh_ref.translation(), [1.0, 2.0, 3.0]);

        let (x, y, z) = Quat::from_array(mesh_ref.rotation()).to_euler(EulerRot::XYZ);
        assert!((x - 4f32.to_radians()).abs() < EPSILON);
        assert!((y - 5f32.to_radians()).abs() < EPSILON);
        assert!((z - 6f32.to_radians()).abs() < EPSILON);

        assert_eq!(mesh_ref.materials().len(), 1);

        let material = pak.read_material_id(mesh_ref.materials()[0]).unwrap();
        assert!(material.alpha_test);
        assert!(material.params.is_some());
        assert_eq!(
            material.params_used,
            MaterialParameterFlags::METAL
                | MaterialParameterFlags::HEIGHT
                | MaterialParameterFlags::TRANSMISSION
        );
        assert!(!material.params_used.contains(MaterialParameterFlags::ROUGH));
    }

    {
        let mesh_ref = find_ref("mesh-with-zero-materials").unwrap();

        assert_eq!(mesh_ref.materials().len(), 0);
    }

    {
        let mesh_ref = find_ref("mesh-with-two-materials-same").unwrap();

        assert_eq!(mesh_ref.materials().len(), 2);
        assert_eq!(mesh_ref.materials()[0], mesh_ref.materials()[1]);
    }

    {
        let mesh_ref = find_ref("mesh-with-two-materials-different").unwrap();

        assert_eq!(mesh_ref.materials().len(), 2);
        assert_ne!(mesh_ref.materials()[0], mesh_ref.materials()[1]);
    }

    {
        let mesh_ref = find_ref("just data").unwrap();

        let my_value = mesh_ref.data("my-value").unwrap();
        assert!(my_value.is_f32());
        assert_eq!(my_value.as_f32(), Some(42.0));

        let another_value = mesh_ref.data("another-value").unwrap();
        assert!(another_value.is_str());
        assert_eq!(another_value.as_str(), Some("foo"));

        let bar = mesh_ref.data("bar").unwrap();
        assert!(bar.is_iter());

        let mut bar_iter = bar.as_iter().unwrap();

        let next = bar_iter.next().unwrap();
        assert!(next.is_i32());
        assert_eq!(next.as_i32(), Some(1));

        let next = bar_iter.next().unwrap();
        assert!(next.is_i32());
        assert_eq!(next.as_i32(), Some(2));

        let next = bar_iter.next().unwrap();
        assert!(next.is_i32());
        assert_eq!(next.as_i32(), Some(3));

        let next = bar_iter.next().unwrap();
        assert!(next.is_str());
        assert_eq!(next.as_str(), Some("banana"));
    }

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn bake_with_generated_content_file() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-generated-content-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;

    let pak_src = generated_dir.join("pak.toml");
    let pak_dst = generated_dir.join("scene.pak");
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['scene/scene.toml']\n",
    )?;

    PakBuf::bake_with_dir(&pak_src, &pak_dst, &*TESTS_DATA_DIR).unwrap();

    let source_files = PakBuf::source_files_with_dir(&pak_src, &*TESTS_DATA_DIR).unwrap();
    assert!(source_files.contains(&pak_src));
    assert!(source_files.contains(&TESTS_DATA_DIR.join("scene/scene.toml")));
    assert!(source_files.contains(&TESTS_DATA_DIR.join("scene/mesh_01.toml")));
    assert!(source_files.contains(&TESTS_DATA_DIR.join("scene/material_01.toml")));

    let mut pak = PakBuf::open(&pak_dst)?;
    pak.read_scene("scene/scene")?;

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn repeated_scene_dependencies_preserve_aliases_and_ids() -> Result<(), Error> {
    let temp = tempfile::tempdir()?;
    let dir = temp.path();
    fs::copy(TESTS_DATA_DIR.join("scene/cube.glb"), dir.join("cube.glb"))?;
    let material = "[material]\ncolor = '#ffffff'\n";
    fs::write(dir.join("material-a.toml"), material)?;
    fs::write(dir.join("material-b.toml"), material)?;
    fs::write(
        dir.join("mesh.toml"),
        "[mesh]\nsrc = 'cube.glb'\ninherit-lods = false\nlods = []\noptimize = false\n",
    )?;

    let inline_mesh = "{ src = 'cube.glb', inherit-lods = false, lods = [], optimize = false }";
    let mut scene = String::from("[scene]\n");
    for index in 0..32 {
        let material = if index == 1 {
            "material-b.toml"
        } else {
            "material-a.toml"
        };
        let mesh = if index == 2 {
            "'mesh.toml'"
        } else {
            inline_mesh
        };
        scene.push_str(&format!(
            "\n[[scene.ref]]\nmaterials = ['{material}']\nmesh = {mesh}\n"
        ));
    }
    fs::write(dir.join("scene.toml"), scene)?;
    fs::write(
        dir.join("pak.toml"),
        "[[content.group]]\nassets = ['scene.toml']\n",
    )?;

    let destination = dir.join("scene.pak");
    PakBuf::bake(dir.join("pak.toml"), &destination).unwrap();
    let mut pak = PakBuf::open(destination)?;
    let scene = pak.read_scene("scene")?;
    let references = scene.refs().collect::<Vec<_>>();
    assert_eq!(references.len(), 32);
    for reference in &references {
        assert_eq!(reference.materials(), references[0].materials());
        assert_eq!(reference.mesh(), references[0].mesh());
    }
    assert_eq!(pak.material_id("material-a"), pak.material_id("material-b"));
    assert_eq!(pak.mesh_id("mesh"), references[0].mesh());
    assert_eq!(pak.material_count(), 1);
    assert_eq!(pak.mesh_count(), 1);

    drop(pak);
    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn transitive_direct_aggregate_uses_its_own_policy_for_dependencies() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-direct-aggregate-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;
    fs::copy(
        TESTS_DATA_DIR.join("scene/cube.glb"),
        generated_dir.join("cube.glb"),
    )?;
    fs::write(generated_dir.join("payload.bin"), b"mesh dependency")?;
    fs::write(
        generated_dir.join("z-mesh.toml"),
        "[mesh]\nsrc = 'cube.glb'\nblob = 'payload.bin'\ninherit-lods = false\nlods = []\noptimize = false\n",
    )?;
    fs::write(
        generated_dir.join("a-scene.toml"),
        "[scene]\n\n[[scene.ref]]\nmesh = 'z-mesh.toml'\n",
    )?;
    let manifest = generated_dir.join("pak.toml");
    fs::write(
        &manifest,
        "[[content.group]]\nname = 'scene'\nsegment = 'outer'\ncompression = 'brotli'\nassets = ['a-scene.toml']\n\n[[content.group]]\nname = 'mesh'\nsegment = 'meshdata'\ncompression = 'none'\nassets = ['z-mesh.toml']\n",
    )?;
    let destination = generated_dir.join("scene.pak");

    PakBuf::bake(&manifest, &destination).unwrap();
    let pak = PakBuf::open(&destination)?;
    let mut dependency = pak.stream_blob("payload.bin")?;
    let mut bytes = Vec::new();
    dependency.read_to_end(&mut bytes)?;
    assert_eq!(bytes, b"mesh dependency");
    drop(dependency);

    let mesh_sidecar = pak
        .segment_file_names()
        .find(|name| name.starts_with("meshdata."))
        .map(|name| generated_dir.join(name))
        .unwrap();
    fs::remove_file(mesh_sidecar)?;
    let mut dependency = pak.stream_blob("payload.bin")?;
    let mut bytes = Vec::new();
    dependency.read_to_end(&mut bytes)?;
    assert_eq!(bytes, b"mesh dependency");

    fs::remove_dir_all(generated_dir)?;
    Ok(())
}
