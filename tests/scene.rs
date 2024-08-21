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

    {
        let model_ref = find_model("just data").unwrap();

        let my_value = model_ref.data("my-value").unwrap();
        assert!(my_value.is_f32());
        assert_eq!(my_value.as_f32(), Some(42.0));

        let another_value = model_ref.data("another-value").unwrap();
        assert!(another_value.is_str());
        assert_eq!(another_value.as_str(), Some("foo"));

        let bar = model_ref.data("bar").unwrap();
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
