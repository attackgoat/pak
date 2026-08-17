#[cfg(feature = "bake")]
use {
    image::{Rgba, RgbaImage},
    pak::{
        Pak, PakBuf,
        bitmap::{BitmapCompression, BitmapFormat},
    },
    std::{fs, io::Error},
};

#[cfg(feature = "bake")]
#[test]
fn compressed_bitmap_bakes_and_round_trips_with_raw_fallback() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-compressed-bitmap-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;

    RgbaImage::from_fn(5, 3, |x, y| Rgba([x as u8, y as u8, 127, 255]))
        .save(generated_dir.join("texture.png"))
        .map_err(Error::other)?;
    fs::write(
        generated_dir.join("texture.toml"),
        "[bitmap]\nsrc = 'texture.png'\ncompression = 'bc1-srgb'\n",
    )?;
    fs::write(
        generated_dir.join("pak.toml"),
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['texture.toml']\n",
    )?;

    PakBuf::bake(
        generated_dir.join("pak.toml"),
        generated_dir.join("texture.pak"),
    )
    .unwrap();

    let mut pak = PakBuf::open(generated_dir.join("texture.pak"))?;
    let info = pak
        .bitmap_info("texture")
        .expect("bitmap info should exist");
    assert_eq!(info.extent(), (5, 3));
    assert_eq!(info.format(), BitmapFormat::Rgb);
    assert_eq!(info.compression(), Some(BitmapCompression::Bc1Srgb));

    let compressed = pak
        .read_compressed_bitmap("texture")?
        .expect("BC variant should be present");
    assert_eq!(compressed.format(), BitmapCompression::Bc1Srgb);
    assert_eq!(compressed.extent(), (5, 3));
    assert_eq!(compressed.mip_levels(), 3);
    assert_eq!(
        compressed
            .mips()
            .iter()
            .map(|mip| mip.extent())
            .collect::<Vec<_>>(),
        [(5, 3), (2, 1), (1, 1)]
    );

    let bitmap = pak.read_bitmap("texture")?;
    assert_eq!(bitmap.extent(), (5, 3));
    assert_eq!(bitmap.format(), BitmapFormat::Rgb);
    assert_eq!(bitmap.pixels().len(), 5 * 3 * 3);
    assert!(bitmap.compressed().is_none());

    fs::remove_dir_all(generated_dir)?;
    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn group_can_disable_texture_compression_default() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-loose-bitmap-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;

    RgbaImage::from_pixel(4, 4, Rgba([17, 34, 51, 255]))
        .save(generated_dir.join("ui.png"))
        .map_err(Error::other)?;
    fs::write(
        generated_dir.join("pak.toml"),
        "[content]\ntexture-compression = true\n\n[[content.group]]\ntexture-compression = false\nassets = ['ui.png']\n",
    )?;

    PakBuf::bake(generated_dir.join("pak.toml"), generated_dir.join("ui.pak")).unwrap();

    let pak = PakBuf::open(generated_dir.join("ui.pak"))?;
    assert_eq!(pak.bitmap_info("ui.png").unwrap().compression(), None);

    fs::remove_dir_all(generated_dir)?;
    Ok(())
}
