#![cfg(feature = "bake")]

use {
    pak::{
        PakBuf,
        buf::{
            DerivedAssetBakeCandidate, DerivedAssetBakeOutput, DerivedAssetBaker, MeshLodRequests,
        },
    },
    std::{
        env,
        ffi::OsStr,
        fs,
        path::Path,
        process::Command,
        sync::mpsc::{Receiver, Sender, channel},
        thread,
        time::Duration,
    },
};

const CHILD_ENV: &str = "PAK_TEST_CARGO_WATCHES_CHILD";
const WAIT: Duration = Duration::from_secs(30);

struct PauseBake {
    entered: Sender<()>,
    resume: Receiver<()>,
}

impl DerivedAssetBaker for PauseBake {
    fn mesh_lod_requests(&self) -> Option<MeshLodRequests> {
        self.entered.send(()).unwrap();
        self.resume.recv_timeout(WAIT).unwrap();
        None
    }

    fn bake(
        &mut self,
        _: &DerivedAssetBakeCandidate<'_>,
    ) -> anyhow::Result<Box<[DerivedAssetBakeOutput]>> {
        unreachable!("the watch test has no opacity micromap candidates")
    }
}

fn prepare(root: &Path) {
    fs::create_dir(root).unwrap();
    fs::write(
        root.join("pak.toml"),
        "[content]\n[[content.group]]\nassets = ['material.toml']\n",
    )
    .unwrap();
    fs::write(
        root.join("material.toml"),
        "[material]\ncolor = 'color.png'\nnormal = 'normal.png'\nrough = 'rough.png'\n",
    )
    .unwrap();
    for name in ["color.png", "normal.png", "rough.png"] {
        image::RgbImage::from_pixel(1, 1, image::Rgb([128, 128, 255]))
            .save(root.join(name))
            .unwrap();
    }
}

#[test]
fn concurrent_bakes_isolate_cargo_watches() {
    if env::var_os(CHILD_ENV).is_some() {
        let dir = tempfile::tempdir().unwrap();
        let enabled = dir.path().join("enabled");
        let disabled = dir.path().join("disabled");
        prepare(&enabled);
        prepare(&disabled);

        let (enabled_entered, enabled_ready) = channel();
        let (resume_enabled, enabled_resume) = channel();
        let (disabled_entered, disabled_ready) = channel();
        let (resume_disabled, disabled_resume) = channel();

        let with_watches = thread::spawn(move || {
            PakBuf::bake_with_dir_and_derived_assets(
                enabled.join("pak.toml"),
                enabled.join("out.pak"),
                enabled,
                &mut PauseBake {
                    entered: enabled_entered,
                    resume: enabled_resume,
                },
            )
            .unwrap();
        });
        enabled_ready.recv_timeout(WAIT).unwrap();

        let without_watches = thread::spawn(move || {
            PakBuf::bake_with_dir_and_derived_assets_without_cargo_watches(
                disabled.join("pak.toml"),
                disabled.join("out.pak"),
                disabled,
                &mut PauseBake {
                    entered: disabled_entered,
                    resume: disabled_resume,
                },
            )
            .unwrap();
        });
        disabled_ready.recv_timeout(WAIT).unwrap();

        // Finish the enabled bake while the disabled one remains active, then let the
        // disabled bake reach the same top-level and nested workers.
        resume_enabled.send(()).unwrap();
        with_watches.join().unwrap();
        resume_disabled.send(()).unwrap();
        without_watches.join().unwrap();
        return;
    }

    let output = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "concurrent_bakes_isolate_cargo_watches",
            "--nocapture",
        ])
        .env(CHILD_ENV, "1")
        .env("CARGO", "1")
        .env("TOKIO_WORKER_THREADS", "1")
        .output()
        .unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success(),
        "{stdout}\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    let watches = stdout
        .lines()
        .filter_map(|line| line.strip_prefix("cargo:rerun-if-changed="))
        .map(Path::new)
        .collect::<Vec<_>>();
    for name in [
        "pak.toml",
        "material.toml",
        "color.png",
        "normal.png",
        "rough.png",
    ] {
        assert!(
            watches
                .iter()
                .any(|path| path.ends_with(Path::new("enabled").join(name))),
            "missing enabled watch for {name}: {watches:?}"
        );
    }
    assert!(
        watches
            .iter()
            .all(|path| path.parent().and_then(Path::file_name) != Some(OsStr::new("disabled"))),
        "disabled bake emitted watches: {watches:?}"
    );
}
