#[cfg(feature = "bake")]
use {
    pak::{PakBuf, buf::SourceAnimation},
    std::{
        fs,
        path::PathBuf,
        time::{SystemTime, UNIX_EPOCH},
    },
};

#[cfg(feature = "bake")]
fn temp_dir(name: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("pak-{name}-{}-{nonce}", std::process::id()))
}

#[cfg(feature = "bake")]
#[test]
fn source_animations_match_manifest_selection_and_are_sorted_and_deduplicated() {
    let generated_dir = temp_dir("source-animations");
    let asset_root = generated_dir.join("assets");
    let animation_dir = asset_root.join("animations");
    let manifest_dir = generated_dir.join("manifest");
    fs::create_dir_all(&animation_dir).unwrap();
    fs::create_dir_all(&manifest_dir).unwrap();

    let a_manifest = animation_dir.join("a.animation.toml");
    let z_manifest = animation_dir.join("z.animation.toml");
    fs::write(
        &a_manifest,
        "[animation]\nsrc = '/models/missing.glb'\nname = 'Walk'\nexclude = ['Finger', 'Toe']\n",
    )
    .unwrap();
    fs::write(&z_manifest, "[animation]\nsrc = 'also-missing.gltf'\n").unwrap();
    fs::write(
        animation_dir.join("excluded.animation.toml"),
        "[animation]\nsrc = 'excluded.glb'\n",
    )
    .unwrap();
    fs::write(
        animation_dir.join("disabled-only.toml"),
        "[animation]\nsrc = 'disabled.glb'\n",
    )
    .unwrap();
    fs::create_dir_all(asset_root.join("models")).unwrap();
    fs::write(asset_root.join("models/direct.glb"), b"not a glb").unwrap();

    let manifest = manifest_dir.join("pak.toml");
    fs::write(
        &manifest,
        "[content]\n\
         [[content.group]]\n\
         assets = ['/animations/*.animation.toml', '/models/direct.glb']\n\
         exclude = ['/animations/excluded.animation.toml']\n\
         [[content.group]]\n\
         assets = ['/animations/a.animation.toml']\n\
         [[content.group]]\n\
         enabled = false\n\
         assets = ['/animations/disabled-only.toml']\n",
    )
    .unwrap();

    let animations: Box<[SourceAnimation]> =
        PakBuf::source_animations_with_dir(&manifest, &asset_root).unwrap();

    assert_eq!(animations.len(), 2);
    assert_eq!(animations[0].key, "animations/a.animation");
    assert_eq!(animations[0].manifest_path, a_manifest);
    assert_eq!(
        animations[0].source_path,
        asset_root.join("models/missing.glb")
    );
    assert!(!animations[0].source_path.exists());
    assert_eq!(animations[0].name.as_deref(), Some("Walk"));
    assert_eq!(animations[0].exclude, ["Finger", "Toe"]);
    assert_eq!(animations[1].key, "animations/z.animation");
    assert_eq!(animations[1].manifest_path, z_manifest);
    assert_eq!(
        animations[1].source_path,
        animation_dir.join("also-missing.gltf")
    );
    assert!(!animations[1].source_path.exists());
    assert_eq!(animations[1].name, None);
    assert!(animations[1].exclude.is_empty());

    fs::remove_dir_all(generated_dir).unwrap();
}

#[cfg(feature = "bake")]
#[test]
fn source_animations_uses_the_manifest_parent_as_the_default_asset_root() {
    let asset_root = temp_dir("source-animations-default-root");
    fs::create_dir_all(&asset_root).unwrap();
    let animation_manifest = asset_root.join("walk.toml");
    fs::write(&animation_manifest, "[animation]\nsrc = 'missing.glb'\n").unwrap();
    let manifest = asset_root.join("pak.toml");
    fs::write(
        &manifest,
        "[content]\n[[content.group]]\nassets = ['walk.toml']\n",
    )
    .unwrap();

    let animations = PakBuf::source_animations(&manifest).unwrap();

    assert_eq!(animations.len(), 1);
    assert_eq!(animations[0].key, "walk");
    assert_eq!(animations[0].manifest_path, animation_manifest);
    assert_eq!(animations[0].source_path, asset_root.join("missing.glb"));
    assert!(!animations[0].source_path.exists());

    fs::remove_dir_all(asset_root).unwrap();
}
