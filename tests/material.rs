#[cfg(feature = "bake")]
use {
    pak::{Pak, PakBuf, bitmap::BitmapFormat},
    std::{fs, io::Error},
};

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
