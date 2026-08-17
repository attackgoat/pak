#![cfg(feature = "bake")]

use {
    pak::{Pak, PakBuf, bitmap::BitmapFormat},
    std::{
        fs,
        io::{Cursor, Error, Read, Seek, SeekFrom, Write},
        path::PathBuf,
        sync::LazyLock,
    },
};

static ROOT: LazyLock<PathBuf> = LazyLock::new(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")));

fn fixture(name: &str) -> Result<(PathBuf, PathBuf), Error> {
    let dir = std::env::temp_dir().join(format!("pak-segment-{name}-{}", std::process::id()));
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("payload.bin"), b"sidecar blob")?;
    fs::copy(
        ROOT.join("tests/data/scene/material_01.png"),
        dir.join("image.png"),
    )?;
    let manifest = dir.join("pak.toml");
    fs::write(
        &manifest,
        "[[content.group]]\nname = 'media'\nsegment = 'media'\ncompression = 'none'\nassets = ['payload.bin', 'image.png']\n",
    )?;
    let pak = dir.join("assets.pak");
    Ok((manifest, pak))
}

fn sidecars(pak: &std::path::Path, segment: &str) -> Result<Vec<PathBuf>, Error> {
    let prefix = format!("{segment}.");
    let mut paths = fs::read_dir(pak.parent().unwrap())?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<Result<Vec<_>, _>>()?;
    paths.retain(|path| {
        path.file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.starts_with(&prefix) && name.ends_with(".pakseg"))
    });
    paths.sort();
    Ok(paths)
}

#[test]
fn named_sidecar_round_trips_blob_and_bitmap() -> Result<(), Error> {
    let (manifest, destination) = fixture("roundtrip")?;
    PakBuf::bake(&manifest, &destination).unwrap();
    assert_eq!(sidecars(&destination, "media")?.len(), 1);

    let mut pak = PakBuf::open(&destination)?;
    let filenames = pak.segment_file_names().collect::<Vec<_>>();
    assert_eq!(filenames.len(), 1);
    assert_eq!(
        filenames[0],
        sidecars(&destination, "media")?[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
    );
    assert!(pak.validate_hash()?);
    assert_eq!(pak.read_blob("payload.bin")?, b"sidecar blob");
    assert_eq!(pak.read_bitmap("image.png")?.format(), BitmapFormat::Rgb);

    let bytes: &'static [u8] = Box::leak(fs::read(&destination)?.into_boxed_slice());
    let error = PakBuf::from_stream(Cursor::new(bytes)).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::Unsupported);

    fs::remove_dir_all(destination.parent().unwrap())?;
    Ok(())
}

#[test]
fn root_can_be_renamed_without_renaming_sidecars() -> Result<(), Error> {
    let (manifest, destination) = fixture("root-rename")?;
    PakBuf::bake(&manifest, &destination).unwrap();
    let expected_filename = sidecars(&destination, "media")?[0]
        .file_name()
        .unwrap()
        .to_str()
        .unwrap()
        .to_owned();
    let renamed = destination.with_file_name("renamed.pak");
    fs::rename(&destination, &renamed)?;

    let mut pak = PakBuf::open(&renamed)?;
    assert_eq!(
        pak.segment_file_names().collect::<Vec<_>>(),
        [expected_filename]
    );
    assert_eq!(pak.read_blob("payload.bin")?, b"sidecar blob");

    fs::remove_dir_all(renamed.parent().unwrap())?;
    Ok(())
}

#[test]
fn unchanged_segment_filename_and_bytes_survive_other_changes() -> Result<(), Error> {
    let dir = std::env::temp_dir().join(format!(
        "pak-segment-independent-generation-{}",
        std::process::id()
    ));
    fs::create_dir_all(&dir)?;
    fs::write(dir.join("root.bin"), b"root one")?;
    fs::write(dir.join("alpha.bin"), b"alpha one")?;
    fs::write(dir.join("beta.bin"), b"beta unchanged")?;
    let manifest = dir.join("pak.toml");
    fs::write(
        &manifest,
        "[[content.group]]\nname = 'root'\ncompression = 'none'\nassets = ['root.bin']\n\n[[content.group]]\nname = 'alpha'\nsegment = 'alpha'\ncompression = 'none'\nassets = ['alpha.bin']\n\n[[content.group]]\nname = 'beta'\nsegment = 'beta'\ncompression = 'none'\nassets = ['beta.bin']\n",
    )?;
    let destination = dir.join("assets.pak");
    PakBuf::bake(&manifest, &destination).unwrap();
    let first = PakBuf::open(&destination)?;
    let first_names = first
        .segment_file_names()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let first_alpha = first_names
        .iter()
        .find(|name| name.starts_with("alpha."))
        .unwrap();
    let first_beta = first_names
        .iter()
        .find(|name| name.starts_with("beta."))
        .unwrap();
    let first_beta_bytes = fs::read(dir.join(first_beta))?;

    fs::write(dir.join("root.bin"), b"root two")?;
    fs::write(dir.join("alpha.bin"), b"alpha two")?;
    PakBuf::bake(&manifest, &destination).unwrap();
    let second = PakBuf::open(&destination)?;
    let second_names = second
        .segment_file_names()
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let second_alpha = second_names
        .iter()
        .find(|name| name.starts_with("alpha."))
        .unwrap();
    let second_beta = second_names
        .iter()
        .find(|name| name.starts_with("beta."))
        .unwrap();

    assert_ne!(first_alpha, second_alpha);
    assert_eq!(first_beta, second_beta);
    assert_eq!(fs::read(dir.join(second_beta))?, first_beta_bytes);

    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
fn missing_and_wrong_sidecars_are_rejected() -> Result<(), Error> {
    let (manifest, destination) = fixture("missing")?;
    PakBuf::bake(&manifest, &destination).unwrap();
    let sidecar = sidecars(&destination, "media")?.pop().unwrap();
    fs::remove_file(&sidecar)?;
    let error = PakBuf::open(&destination).unwrap_err();
    assert_eq!(error.kind(), std::io::ErrorKind::NotFound);
    assert!(error.to_string().contains("media"));

    PakBuf::bake(&manifest, &destination).unwrap();
    let mut file = fs::OpenOptions::new().write(true).open(&sidecar)?;
    file.write_all(b"WRONG")?;
    drop(file);
    assert_eq!(
        PakBuf::open(&destination).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    assert_eq!(
        PakBuf::bake(&manifest, &destination)
            .unwrap_err()
            .root_cause()
            .downcast_ref::<Error>()
            .unwrap()
            .kind(),
        std::io::ErrorKind::AlreadyExists
    );

    fs::remove_dir_all(destination.parent().unwrap())?;

    let (manifest, destination) = fixture("truncated")?;
    PakBuf::bake(&manifest, &destination).unwrap();
    let sidecar = sidecars(&destination, "media")?.pop().unwrap();
    fs::OpenOptions::new()
        .write(true)
        .open(&sidecar)?
        .set_len(5)?;
    assert_eq!(
        PakBuf::open(&destination).unwrap_err().kind(),
        std::io::ErrorKind::InvalidData
    );
    fs::remove_dir_all(destination.parent().unwrap())?;
    Ok(())
}

#[test]
fn sidecar_hash_validation_detects_corruption() -> Result<(), Error> {
    let (manifest, destination) = fixture("hash")?;
    PakBuf::bake(&manifest, &destination).unwrap();
    let sidecar = sidecars(&destination, "media")?.pop().unwrap();
    let pak = PakBuf::open(&destination)?;
    assert!(pak.validate_hash()?);

    let mut file = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(&sidecar)?;
    file.seek(SeekFrom::End(-9))?;
    let mut byte = [0];
    file.read_exact(&mut byte)?;
    file.seek(SeekFrom::End(-9))?;
    file.write_all(&[byte[0].wrapping_add(1)])?;
    drop(file);
    assert!(!pak.validate_hash()?);

    fs::remove_dir_all(destination.parent().unwrap())?;
    Ok(())
}

#[test]
fn generation_sidecars_are_reused_and_open_handles_keep_their_snapshot() -> Result<(), Error> {
    let (manifest, destination) = fixture("generation")?;
    PakBuf::bake(&manifest, &destination).unwrap();
    let first_sidecar = sidecars(&destination, "media")?.pop().unwrap();
    let first_bytes = fs::read(&first_sidecar)?;
    let mut old = PakBuf::open(&destination)?;

    PakBuf::bake(&manifest, &destination).unwrap();
    assert_eq!(sidecars(&destination, "media")?, [first_sidecar.clone()]);
    assert_eq!(fs::read(&first_sidecar)?, first_bytes);

    fs::write(
        destination.parent().unwrap().join("payload.bin"),
        b"new generation",
    )?;
    PakBuf::bake(&manifest, &destination).unwrap();
    assert_eq!(sidecars(&destination, "media")?.len(), 2);

    assert_eq!(old.read_blob("payload.bin")?, b"sidecar blob");
    let mut old_stream = old.stream_blob("payload.bin")?;
    let mut old_bytes = Vec::new();
    old_stream.read_to_end(&mut old_bytes)?;
    assert_eq!(old_bytes, b"sidecar blob");
    assert!(old.validate_hash()?);

    let mut new = PakBuf::open(&destination)?;
    assert_eq!(new.read_blob("payload.bin")?, b"new generation");

    fs::remove_dir_all(destination.parent().unwrap())?;
    Ok(())
}

#[test]
fn sidecar_id_and_generation_mismatches_are_rejected() -> Result<(), Error> {
    for (fixture_name, offset) in [("id-mismatch", 20_u64), ("generation-mismatch", 35_u64)] {
        let (manifest, destination) = fixture(fixture_name)?;
        PakBuf::bake(&manifest, &destination).unwrap();
        let sidecar = sidecars(&destination, "media")?.pop().unwrap();
        let mut file = fs::OpenOptions::new().write(true).open(sidecar)?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&[0xff])?;
        drop(file);

        assert_eq!(
            PakBuf::open(&destination).unwrap_err().kind(),
            std::io::ErrorKind::InvalidData
        );
        fs::remove_dir_all(destination.parent().unwrap())?;
    }
    Ok(())
}
