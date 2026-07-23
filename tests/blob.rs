#[cfg(feature = "bake")]
use {
    pak::{BlobId, Pak, PakBuf},
    std::{
        fs,
        io::{Error, Read, Seek, SeekFrom, Write},
        path::PathBuf,
        sync::LazyLock,
    },
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
    assert!(pak.validate_hash()?);
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
    assert!(pak.validate_hash()?);
    assert_eq!(pak.blob_count(), 1);
    assert_eq!(pak.read_blob("blob/payload")?, b"blob payload\n".to_vec());

    fs::remove_dir_all(generated_dir)?;

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn hash_validation_is_explicit() -> Result<(), Error> {
    let generated_dir =
        std::env::temp_dir().join(format!("pak-hash-validation-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;

    let pak_src = generated_dir.join("pak.toml");
    let pak_dst = generated_dir.join("blob.pak");
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['/blob/payload.toml']\n",
    )?;

    PakBuf::bake_with_dir(&pak_src, &pak_dst, &*TESTS_DATA_DIR).unwrap();
    assert!(PakBuf::open(&pak_dst)?.validate_hash()?);

    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&pak_dst)?;
    file.seek(SeekFrom::End(-1))?;
    let mut byte = [0];
    file.read_exact(&mut byte)?;
    file.seek(SeekFrom::End(-1))?;
    file.write_all(&[byte[0].wrapping_add(1)])?;
    drop(file);

    let pak = PakBuf::open(&pak_dst)?;
    assert!(!pak.validate_hash()?);

    fs::remove_dir_all(generated_dir)?;

    Ok(())
}

#[cfg(feature = "bake")]
#[test]
fn repeated_bakes_assign_stable_blob_ids() -> Result<(), Error> {
    let generated_dir = std::env::temp_dir().join(format!("pak-stable-ids-{}", std::process::id()));
    fs::create_dir_all(&generated_dir)?;
    for idx in 0..32 {
        fs::write(
            generated_dir.join(format!("asset_{idx:02}.bin")),
            vec![idx as u8; idx * 4096 + 1],
        )?;
    }
    let pak_src = generated_dir.join("pak.toml");
    fs::write(
        &pak_src,
        "[content]\ncompression = 'snap'\n\n[[content.group]]\nassets = ['*.bin']\n",
    )?;

    let mut expected_bytes = None;
    for bake_idx in 0..3 {
        let pak_dst = generated_dir.join(format!("blob-{bake_idx}.pak"));
        PakBuf::bake(&pak_src, &pak_dst).unwrap();
        let pak = PakBuf::open(&pak_dst)?;
        for idx in 0..32 {
            assert_eq!(
                pak.blob_id(&format!("asset_{idx:02}.bin")),
                Some(BlobId(idx))
            );
        }

        let bytes = fs::read(pak_dst)?;
        if let Some(expected_bytes) = &expected_bytes {
            assert_eq!(&bytes, expected_bytes);
        } else {
            expected_bytes = Some(bytes);
        }
    }

    fs::remove_dir_all(generated_dir)?;
    Ok(())
}
