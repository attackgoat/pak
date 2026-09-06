#[cfg(feature = "bake")]
use {
    pak::{
        MaterialParameterFlags, Pak, PakBuf,
        bitmap::{BitmapCompression, BitmapFormat},
    },
    std::{fs, io::Error, path::PathBuf, sync::LazyLock},
};

#[cfg(feature = "bake")]
static CARGO_MANIFEST_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
#[cfg(feature = "bake")]
static TESTS_DATA_DIR: LazyLock<PathBuf> = LazyLock::new(|| CARGO_MANIFEST_DIR.join("tests/data"));

#[cfg(feature = "bake")]
#[test]
fn default_material_color_bitmap_matches_format() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-default-material-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;

    let material_src = generated_dir.join("material.toml");
    let pak_src = generated_dir.join("pak.toml");
    let pak_dst = generated_dir.join("material.pak");
    fs::write(&material_src, "[material]\n")?;
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['material.toml']\n",
    )?;

    PakBuf::bake(&pak_src, &pak_dst).unwrap();

    let mut pak = PakBuf::open(&pak_dst)?;
    let material = pak.read_material("material").unwrap();
    assert_eq!(material.data, pak::scene::DataMap::default());
    assert!(material.data("missing").is_none());
    let color = pak.read_bitmap_id(material.color)?;

    assert_eq!(color.format(), BitmapFormat::Rgba);
    assert_eq!(color.extent(), (1, 1));
    assert_eq!(color.pixels().len(), 4);

    fs::remove_dir_all(generated_dir)?;

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn material_bitmap_toml_src_resolves_relative_to_bitmap_toml() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-material-bitmap-{}", std::process::id()));
    let materials_dir = generated_dir.join("materials");
    let textures_dir = generated_dir.join("textures");
    fs::create_dir_all(&materials_dir)?;
    fs::create_dir_all(&textures_dir)?;

    fs::copy(
        TESTS_DATA_DIR.join("scene/material_01.png"),
        textures_dir.join("albedo.png"),
    )?;
    fs::write(
        textures_dir.join("albedo.toml"),
        "[bitmap]\nsrc = 'albedo.png'\n",
    )?;
    fs::write(
        materials_dir.join("mat.toml"),
        "[material]\ncolor = '../textures/albedo.toml'\n",
    )?;

    let pak_src = generated_dir.join("pak.toml");
    let pak_dst = generated_dir.join("material.pak");
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\ntexture-compression = true\n\n[[content.group]]\nassets = ['materials/mat.toml', 'textures/albedo.toml']\n",
    )?;

    let source_files = PakBuf::source_files(&pak_src).unwrap();
    assert!(source_files.contains(&textures_dir.join("albedo.toml")));
    assert!(source_files.contains(&textures_dir.join("albedo.png")));

    PakBuf::bake(&pak_src, &pak_dst).unwrap();

    let mut pak = PakBuf::open(&pak_dst)?;
    let material = pak.read_material("materials/mat").unwrap();
    assert_eq!(pak.bitmap_count(), 1);
    assert_eq!(pak.bitmap_id("textures/albedo"), Some(material.color));
    let color_info = pak.bitmap_info_id(material.color).unwrap();
    assert_eq!(color_info.compression(), Some(BitmapCompression::Bc1Srgb));
    assert!(pak.read_compressed_bitmap_id(material.color)?.is_some());
    let color = pak.read_bitmap_id(material.color)?;
    assert!(!color.pixels().is_empty());

    fs::remove_dir_all(generated_dir)?;

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn material_occlusion_path_is_a_source_dependency() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-material-occlusion-src-{}", std::process::id()));
    let materials_dir = generated_dir.join("materials");
    let textures_dir = generated_dir.join("textures");
    fs::create_dir_all(&materials_dir)?;
    fs::create_dir_all(&textures_dir)?;

    fs::copy(
        TESTS_DATA_DIR.join("scene/material_01.png"),
        textures_dir.join("ao.png"),
    )?;
    fs::write(
        materials_dir.join("mat.toml"),
        "[material]\nocclusion = '../textures/ao.png'\n",
    )?;

    let pak_src = generated_dir.join("pak.toml");
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['materials/mat.toml']\n",
    )?;

    let source_files = PakBuf::source_files(&pak_src).unwrap();
    assert!(source_files.contains(&textures_dir.join("ao.png")));

    fs::remove_dir_all(generated_dir)?;

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn material_occlusion_uses_parameter_b_and_flag() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-material-occlusion-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;

    let material_src = generated_dir.join("material.toml");
    let pak_src = generated_dir.join("pak.toml");
    let pak_dst = generated_dir.join("material.pak");
    fs::write(&material_src, "[material]\nocclusion = 0.25\n")?;
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['material.toml']\n",
    )?;

    PakBuf::bake(&pak_src, &pak_dst).unwrap();

    let mut pak = PakBuf::open(&pak_dst)?;
    let material = pak.read_material("material").unwrap();
    assert_eq!(material.params_used, MaterialParameterFlags::OCCLUSION);
    let params = pak.read_bitmap_id(material.params.unwrap())?;
    assert_eq!(params.format(), BitmapFormat::Rgba);
    assert_eq!(params.extent(), (1, 1));
    assert_eq!(params.pixels(), &[0, u8::MAX, 63, 0]);

    fs::remove_dir_all(generated_dir)?;

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn material_landscape_uses_flag_without_params_bitmap() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-material-landscape-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;

    let material_src = generated_dir.join("material.toml");
    let pak_src = generated_dir.join("pak.toml");
    let pak_dst = generated_dir.join("material.pak");
    fs::write(&material_src, "[material]\nlandscape = true\n")?;
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['material.toml']\n",
    )?;

    PakBuf::bake(&pak_src, &pak_dst).unwrap();

    let pak = PakBuf::open(&pak_dst)?;
    let material = pak.read_material("material").unwrap();
    assert!(
        material
            .params_used
            .contains(MaterialParameterFlags::LANDSCAPE)
    );
    assert!(material.params.is_none());

    fs::remove_dir_all(generated_dir)?;

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn material_rejects_height_with_occlusion() -> Result<(), Error> {
    let generated_dir = std::env::temp_dir().join(format!(
        "pak-material-height-occlusion-{}",
        std::process::id()
    ));
    fs::create_dir_all(&generated_dir)?;

    let material_src = generated_dir.join("material.toml");
    let pak_src = generated_dir.join("pak.toml");
    let pak_dst = generated_dir.join("material.pak");
    fs::write(
        &material_src,
        "[material]\nheight = 0.25\nocclusion = 0.75\n",
    )?;
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['material.toml']\n",
    )?;

    let error = PakBuf::bake(&pak_src, &pak_dst).expect_err("conflicting B inputs should fail");
    assert!(format!("{error:#}").contains(
        "material cannot specify both height and occlusion because they share parameter B"
    ));

    fs::remove_dir_all(generated_dir)?;

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn material_data_bakes_round_trips_and_deduplicates() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-material-data-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;

    let material_src = generated_dir.join("material.toml");
    let pak_src = generated_dir.join("pak.toml");
    let pak_dst = generated_dir.join("material.pak");
    fs::write(
        &material_src,
        "[material.data]\nz = 'label'\na = [true, 0.25, -7, 'label', [[], ['nested']]]\n",
    )?;
    fs::write(
        generated_dir.join("equal.toml"),
        "[material.data]\na = [true, 0.25, -7, 'label', [[], ['nested']]]\nz = 'label'\n",
    )?;
    fs::write(
        generated_dir.join("distinct.toml"),
        "[material.data]\nz = 'different'\na = [true, 0.25, -7, 'label', [[], ['nested']]]\n",
    )?;
    fs::write(
        generated_dir.join("scene.toml"),
        "[[scene.ref]]\nmaterials = [{ data = { z = 'label', a = [true, 0.25, -7, 'label', [[], ['nested']]] } }]\n",
    )?;
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['material.toml', 'equal.toml', 'distinct.toml', 'scene.toml']\n",
    )?;

    PakBuf::bake(&pak_src, &pak_dst).unwrap();

    let mut pak = PakBuf::open(&pak_dst)?;
    let material = pak.read_material("material").unwrap();
    assert_eq!(pak.material_id("material"), pak.material_id("equal"));
    assert_ne!(pak.material_id("material"), pak.material_id("distinct"));
    assert_eq!(pak.material_count(), 2);
    assert_eq!(pak.bitmap_count(), 1);
    let scene = pak.read_scene("scene")?;
    assert_eq!(
        scene.refs().next().unwrap().materials(),
        &[pak.material_id("material").unwrap()]
    );
    assert_eq!(material, pak.read_material("equal").unwrap());
    assert_eq!(
        material.data.iter().map(|(key, _)| key).collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert_eq!(material.data("z").unwrap().expect_str(), "label");
    assert!(material.data("missing").is_none());
    let mut values = material.data("a").unwrap().expect_iter();
    assert_eq!(values.len(), 5);
    assert!(values.next().unwrap().expect_bool());
    assert_eq!(values.next().unwrap().expect_f32(), 0.25);
    assert_eq!(values.next().unwrap().expect_i32(), -7);
    assert_eq!(values.next().unwrap().expect_str(), "label");
    let mut nested = values.next().unwrap().expect_iter();
    assert_eq!(nested.next().unwrap().expect_iter().len(), 0);
    assert_eq!(
        nested
            .next()
            .unwrap()
            .expect_iter()
            .next()
            .unwrap()
            .expect_str(),
        "nested"
    );
    assert!(values.next().is_none());

    fs::remove_dir_all(generated_dir)?;
    Ok(())
}

#[test]
fn material_data_map_construction_is_canonical_without_bake() {
    use pak::scene::{DataData, DataMap};

    let entries = [
        ("z".to_owned(), DataData::String("a".to_owned())),
        (
            "a".to_owned(),
            DataData::Array(vec![
                DataData::Bool(true),
                DataData::Float(0.25),
                DataData::Number(-7),
                DataData::Array(vec![DataData::String("z".to_owned())]),
            ]),
        ),
    ];
    let map: DataMap = entries.clone().into_iter().collect();
    let reversed: DataMap = entries.into_iter().rev().collect();
    assert_eq!(map, reversed);
    assert_eq!(
        map.iter().map(|(key, _)| key).collect::<Vec<_>>(),
        ["a", "z"]
    );
    assert_eq!(map.get("z").unwrap().expect_str(), "a");
    assert!(map.get("missing").is_none());
    let bytes = bincode::serde::encode_to_vec(&map, bincode::config::standard()).unwrap();
    assert_eq!(
        bytes,
        bincode::serde::encode_to_vec(&reversed, bincode::config::standard()).unwrap()
    );
    let (decoded, consumed): (DataMap, _) =
        bincode::serde::decode_from_slice(&bytes, bincode::config::standard()).unwrap();
    assert_eq!(consumed, bytes.len());
    assert_eq!(decoded, map);
    assert_eq!(DataMap::default().iter().count(), 0);
    assert!(DataMap::default().get("missing").is_none());
    let duplicates: DataMap = [
        ("key".to_owned(), DataData::Number(1)),
        ("key".to_owned(), DataData::Number(2)),
    ]
    .into_iter()
    .collect();
    assert_eq!(duplicates.get("key").unwrap().expect_i32(), 2);
}
