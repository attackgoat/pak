#![cfg(feature = "bake")]

use {
    gltf::binary::{Glb, Header},
    pak::{
        Pak as _, PakBuf,
        anim::{Interpolation, Outputs},
    },
    serde_json::{Value, json},
    std::{borrow::Cow, fs, path::Path},
};

fn write_source(path: &Path, document: &Value, bytes: &[u8]) {
    let json = serde_json::to_vec(document).unwrap();
    if path.extension().unwrap() == "glb" {
        let glb = Glb {
            header: Header {
                magic: *b"glTF",
                version: 2,
                length: 0,
            },
            json: Cow::Borrowed(&json),
            bin: Some(Cow::Borrowed(bytes)),
        };
        fs::write(path, glb.to_vec().unwrap()).unwrap();
    } else {
        fs::write(path.with_extension("bin"), bytes).unwrap();
        fs::write(path, json).unwrap();
    }
}

#[test]
fn animation_import_skips_images_and_preserves_channels_and_archive_bytes() {
    for extension in ["gltf", "glb"] {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let source = root.join(format!("source.{extension}"));
        let bytes = [0.0_f32, 1.0, 0.0, 0.0, 0.0, 1.0, 2.0, 3.0]
            .into_iter()
            .flat_map(f32::to_le_bytes)
            .chain(*b"bad!")
            .collect::<Vec<_>>();
        let mut document = json!({
            "asset": {"version": "2.0"},
            "buffers": [{"byteLength": bytes.len()}],
            "bufferViews": [
                {"buffer": 0, "byteLength": 8},
                {"buffer": 0, "byteOffset": 8, "byteLength": 24},
                {"buffer": 0, "byteOffset": 32, "byteLength": 4},
            ],
            "accessors": [
                {
                    "bufferView": 0, "componentType": 5126, "count": 2, "type": "SCALAR",
                    "min": [0], "max": [1],
                },
                {"bufferView": 1, "componentType": 5126, "count": 2, "type": "VEC3"},
            ],
            "nodes": [{"name": "Root"}, {"name": "Excluded"}],
            "animations": [
                {
                    "name": "Idle",
                    "samplers": [{"input": 0, "output": 1, "interpolation": "STEP"}],
                    "channels": [{"sampler": 0, "target": {"node": 0, "path": "translation"}}],
                },
                {
                    "name": "Walk",
                    "samplers": [{"input": 0, "output": 1}],
                    "channels": [
                        {"sampler": 0, "target": {"node": 0, "path": "translation"}},
                        {"sampler": 0, "target": {"node": 1, "path": "translation"}},
                    ],
                },
            ],
        });
        if extension == "gltf" {
            document["buffers"][0]["uri"] = json!("source.bin");
        }

        write_source(&source, &document, &bytes);
        fs::write(
            root.join("first.toml"),
            format!("[animation]\nsrc = 'source.{extension}'\n"),
        )
        .unwrap();
        fs::write(
            root.join("walk.toml"),
            format!(
                "[animation]\nsrc = 'source.{extension}'\nname = 'Walk'\nexclude = ['Excluded']\n"
            ),
        )
        .unwrap();
        let manifest = root.join("pak.toml");
        fs::write(
            &manifest,
            "[[content.group]]\nassets = ['first.toml', 'walk.toml']\n",
        )
        .unwrap();
        let original = root.join("original.pak");
        PakBuf::bake_with_dir_without_cargo_watches(&manifest, &original, root).unwrap();

        document["images"] = if extension == "glb" {
            json!([{"bufferView": 2, "mimeType": "image/png"}])
        } else {
            fs::write(root.join("invalid.png"), b"not a png").unwrap();
            json!([{"uri": "missing.png"}, {"uri": "invalid.png"}])
        };
        write_source(&source, &document, &bytes);
        assert!(gltf::import(&source).is_err());

        let output = root.join("images.pak");
        PakBuf::bake_with_dir_without_cargo_watches(&manifest, &output, root).unwrap();
        assert_eq!(fs::read(original).unwrap(), fs::read(&output).unwrap());

        let mut pak = PakBuf::open(output).unwrap();
        for (key, interpolation) in [
            ("first", Interpolation::Step),
            ("walk", Interpolation::Linear),
        ] {
            let animation = pak.read_animation(key).unwrap();
            assert_eq!(animation.channels().len(), 1);
            let channel = &animation.channels()[0];
            assert_eq!(channel.target(), "Root");
            assert_eq!(channel.inputs(), [0, 1000]);
            assert_eq!(channel.interpolation(), interpolation);
            assert_eq!(
                channel.outputs(),
                &Outputs::Translations(vec![[0.0, 0.0, 0.0], [1.0, 2.0, 3.0]])
            );
        }
    }
}
