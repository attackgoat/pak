#[cfg(feature = "bake")]
use {
    pak::{Pak, PakBuf, bitmap::BitmapFormat},
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
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['materials/mat.toml']\n",
    )?;

    PakBuf::bake(&pak_src, &pak_dst).unwrap();

    let mut pak = PakBuf::open(&pak_dst)?;
    let material = pak.read_material("materials/mat").unwrap();
    let color = pak.read_bitmap_id(material.color)?;
    assert!(!color.pixels().is_empty());

    fs::remove_dir_all(generated_dir)?;

    Ok(())
}
