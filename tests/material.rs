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
