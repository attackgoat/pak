use {
    glam::{EulerRot, Quat},
    lazy_static::lazy_static,
    pak::{Pak, PakBuf},
    std::{io::Error, path::PathBuf},
};

const EPSILON: f32 = 0.0001;

lazy_static! {
    static ref CARGO_MANIFEST_DIR: PathBuf = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    static ref TESTS_DATA_DIR: PathBuf = CARGO_MANIFEST_DIR.join("tests/data");
}

#[test]
fn deserialize_scene_materials() -> Result<(), Error> {
    let pak_dst = TESTS_DATA_DIR.join("scene/test.pak");

    {
        let pak_src = TESTS_DATA_DIR.join("scene/pak.toml");
        PakBuf::bake(&pak_src, &pak_dst).unwrap();
    }

    let mut pak = PakBuf::open(&pak_dst)?;
    let scene_01 = pak.read_scene("scene")?;
    let find_model = |id| scene_01.refs().find(|r| r.id() == Some(id));

    {
        let model_ref = find_model("model-with-one-material").unwrap();

        assert_eq!(model_ref.position(), [1.0, 2.0, 3.0]);

        let (x, y, z) = Quat::from_array(model_ref.rotation()).to_euler(EulerRot::XYZ);
        assert!((x - 4f32.to_radians()).abs() < EPSILON);
        assert!((y - 5f32.to_radians()).abs() < EPSILON);
        assert!((z - 6f32.to_radians()).abs() < EPSILON);

        assert_eq!(model_ref.materials().len(), 1);
    }

    {
        let model_ref = find_model("model-with-zero-materials").unwrap();

        assert_eq!(model_ref.materials().len(), 0);
    }

    {
        let model_ref = find_model("model-with-two-materials-same").unwrap();

        assert_eq!(model_ref.materials().len(), 2);
        assert_eq!(model_ref.materials()[0], model_ref.materials()[1]);
    }

    {
        let model_ref = find_model("model-with-two-materials-different").unwrap();

        assert_eq!(model_ref.materials().len(), 2);
        assert_ne!(model_ref.materials()[0], model_ref.materials()[1]);
    }

    Ok(())
}
