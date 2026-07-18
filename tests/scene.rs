use {
    glam::{EulerRot, Quat},
    pak::{MaterialParameterFlags, Pak, PakBuf},
    std::{io::Error, path::PathBuf, sync::LazyLock},
};

#[cfg(feature = "bake")]
use std::fs;

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
