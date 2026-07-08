#[cfg(feature = "bake")]
use {
    pak::{Pak, PakBuf},
    std::{fs, io::Error, path::PathBuf, sync::LazyLock},
};

#[cfg(feature = "bake")]
static CARGO_MANIFEST_DIR: LazyLock<PathBuf> =
    LazyLock::new(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));
#[cfg(feature = "bake")]
static TESTS_DATA_DIR: LazyLock<PathBuf> = LazyLock::new(|| CARGO_MANIFEST_DIR.join("tests/data"));

#[cfg(feature = "bake")]
#[test]
fn bake_blob_asset_toml() -> Result<(), Error> {
    let data_dir = TESTS_DATA_DIR.join("blob");
    let pak_src = data_dir.join("pak.toml");
    let pak_dst = std::env::temp_dir().join(format!("pak-blob-{}.pak", std::process::id()));

    PakBuf::bake(&pak_src, &pak_dst).unwrap();

    let source_files = PakBuf::source_files(&pak_src).unwrap();
    assert!(source_files.contains(&pak_src));
    assert!(source_files.contains(&data_dir.join("payload.toml")));
    assert!(source_files.contains(&data_dir.join("payload.bin")));
    assert!(source_files.contains(&data_dir.join("payload.name.bin.toml")));
    assert!(source_files.contains(&data_dir.join("payload.name.bin")));

    let mut pak = PakBuf::open(&pak_dst)?;
    assert_eq!(pak.blob_count(), 2);
    assert_eq!(pak.read_blob("payload")?, b"blob payload\n".to_vec());
    assert_eq!(
        pak.read_blob("payload.name.bin")?,
        b"dotted blob payload\n".to_vec()
    );

    fs::remove_file(pak_dst)?;

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn bake_with_dir_resolves_project_rooted_asset_globs() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-project-rooted-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;

    let pak_src = generated_dir.join("pak.toml");
    let pak_dst = generated_dir.join("blob.pak");
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['/blob/payload.toml']\n",
    )?;

    PakBuf::bake_with_dir(&pak_src, &pak_dst, &*TESTS_DATA_DIR).unwrap();

    let source_files = PakBuf::source_files_with_dir(&pak_src, &*TESTS_DATA_DIR).unwrap();
    assert!(source_files.contains(&pak_src));
    assert!(source_files.contains(&TESTS_DATA_DIR.join("blob/payload.toml")));
    assert!(source_files.contains(&TESTS_DATA_DIR.join("blob/payload.bin")));

    let mut pak = PakBuf::open(&pak_dst)?;
    assert_eq!(pak.blob_count(), 1);
    assert_eq!(pak.read_blob("blob/payload")?, b"blob payload\n".to_vec());

    fs::remove_dir_all(generated_dir)?;

    Ok(())
}
