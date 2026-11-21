#![cfg(feature = "bake")]

use {
    pak::{Pak, PakBuf, bitmap::BitmapFormat},
    std::{
        fs,
        io::{Cursor, Error, Read},
        path::PathBuf,
        process::Command,
        sync::LazyLock,
    },
};

#[cfg(debug_assertions)]
use std::io::{Seek, SeekFrom, Write};

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
        "[[content.group]]\nname = 'media'\ncompression = 'none'\nassets = ['payload.bin', 'image.png']\n",
    )?;
    let pak = dir.join("assets.pak");
    Ok((manifest, pak))
}

#[cfg(debug_assertions)]
fn sidecars(pak: &std::path::Path, segment: &str) -> Result<Vec<PathBuf>, Error> {
    let prefix = format!("group-0-{segment}.");
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
#[cfg(debug_assertions)]
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
#[cfg(debug_assertions)]
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
#[cfg(debug_assertions)]
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
        "[[content.group]]\nname = 'root'\ncompression = 'none'\nassets = ['root.bin']\n\n[[content.group]]\nname = 'alpha'\ncompression = 'none'\nassets = ['alpha.bin']\n\n[[content.group]]\nname = 'beta'\ncompression = 'none'\nassets = ['beta.bin']\n",
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
        .find(|name| name.starts_with("group-1-alpha."))
        .unwrap();
    let first_beta = first_names
        .iter()
        .find(|name| name.starts_with("group-2-beta."))
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
        .find(|name| name.starts_with("group-1-alpha."))
        .unwrap();
    let second_beta = second_names
        .iter()
        .find(|name| name.starts_with("group-2-beta."))
        .unwrap();

    assert_ne!(first_alpha, second_alpha);
    assert_eq!(first_beta, second_beta);
    assert_eq!(fs::read(dir.join(second_beta))?, first_beta_bytes);

    fs::remove_dir_all(dir)?;
    Ok(())
}

#[test]
#[cfg(debug_assertions)]
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
#[cfg(debug_assertions)]
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
#[cfg(debug_assertions)]
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
#[cfg(debug_assertions)]
fn sidecar_id_and_generation_mismatches_are_rejected() -> Result<(), Error> {
    for (fixture_name, offset) in [("id-mismatch", 20_u64), ("generation-mismatch", 43_u64)] {
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

#[test]
#[ignore = "subprocess fixture for cargo_profile_controls_layout_without_changing_assets"]
fn bake_profile_fixture() -> Result<(), Error> {
    let root = PathBuf::from(std::env::var_os("PAK_TEST_PROFILE_ROOT").unwrap());
    PakBuf::bake_with_dir_without_cargo_watches(
        root.join("pak.toml"),
        root.join("assets.pak"),
        &root,
    )
    .map_err(Error::other)
}

#[test]
fn cargo_profile_controls_layout_without_changing_assets() -> Result<(), Error> {
    let mut expected = None;
    for (name, profile, debug_bake) in [
        ("debug", Some("debug"), true),
        ("release", Some("release"), false),
        ("standalone", None, cfg!(debug_assertions)),
    ] {
        let (manifest, destination) = fixture(&format!("profile-{name}"))?;
        let root = manifest.parent().unwrap();
        fs::write(
            root.join("compressed.bin"),
            b"compressed payload".repeat(64),
        )?;
        fs::write(
            &manifest,
            "[content]\ncompression = 'snap'\ntexture-compression = true\n\n[[content.group]]\nname = 'media'\ncompression = 'none'\ntexture-compression = false\nassets = ['payload.bin', 'image.png']\n\n[[content.group]]\nname = 'compressed'\ncompression = 'brotli'\nassets = ['compressed.bin']\n",
        )?;
        // A child process exercises the public entry point without mutating this process's env.
        let mut command = Command::new(std::env::current_exe()?);
        command
            .args(["--ignored", "--exact", "bake_profile_fixture"])
            .env("PAK_TEST_PROFILE_ROOT", root)
            .env_remove("PROFILE");
        if let Some(profile) = profile {
            command.env("PROFILE", profile);
        }
        let output = command.output()?;
        assert!(
            output.status.success(),
            "{name} bake failed: {}\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );

        let mut pak = PakBuf::open(&destination)?;
        assert!(pak.validate_hash()?);
        assert_eq!(
            pak.segment_file_names().count(),
            if debug_bake { 2 } else { 0 }
        );
        let sidecars = fs::read_dir(root)?
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<Result<Vec<_>, _>>()?
            .into_iter()
            .filter(|path| {
                path.extension()
                    .is_some_and(|extension| extension == "pakseg")
            })
            .count();
        assert_eq!(sidecars, if debug_bake { 2 } else { 0 });

        let mut keys = pak.keys().map(str::to_owned).collect::<Vec<_>>();
        keys.sort();
        let bitmap = pak.read_bitmap("image.png")?;
        assert_eq!(bitmap.format(), BitmapFormat::Rgb);
        let snapshot = (
            keys,
            pak.blob_id("payload.bin"),
            pak.bitmap_id("image.png"),
            pak.read_blob("compressed.bin")?,
            bitmap.pixels().to_vec(),
        );
        if let Some(expected) = &expected {
            assert_eq!(&snapshot, expected);
        } else {
            expected = Some(snapshot);
        }
        let mut uncompressed = Vec::new();
        pak.stream_blob("payload.bin")?
            .read_to_end(&mut uncompressed)?;
        assert_eq!(uncompressed, b"sidecar blob");
        assert!(pak.stream_blob("compressed.bin").is_err());

        if !debug_bake {
            let bytes: &'static [u8] = Box::leak(fs::read(&destination)?.into_boxed_slice());
            let mut standalone = PakBuf::from_stream(Cursor::new(bytes))?;
            assert_eq!(standalone.read_blob("payload.bin")?, b"sidecar blob");
        }
        fs::remove_dir_all(root)?;
    }
    Ok(())
}
