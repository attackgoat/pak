//! Contains functions and types used to bake assets into .pak files
//!
//! Assets are regular art such as `.glb`, `.jpeg` and `.ttf` files.

mod anim;
mod asset;
mod bitmap;
mod blob;
mod content;
mod material;
mod model;
mod scene;
mod writer;

use {
    self::{asset::Asset, bitmap::Bitmap, blob::Blob, model::Model, writer::Writer},
    super::{
        compression::Compression, AnimationBuf, AnimationId, BitmapBuf, BitmapFontBuf,
        BitmapFontId, BitmapId, BlobId, MaterialId, MaterialInfo, ModelBuf, ModelId, Pak, SceneBuf,
        SceneId,
    },
    crate::{Data, DataRef, Id, PakBuf, PakFile, Stream},
    anyhow::Context,
    glob::glob,
    log::{error, info, trace, warn},
    parking_lot::Mutex,
    serde::{de::DeserializeOwned, Deserialize, Serialize},
    std::{
        collections::HashMap,
        env::var,
        fmt::{Debug, Formatter},
        fs::{create_dir_all, File},
        io::{BufReader, Cursor, Error, ErrorKind, Read, Seek, SeekFrom},
        ops::Range,
        path::{Path, PathBuf},
        sync::Arc,
        u32,
    },
    tokio::runtime::Runtime,
};

/// Given some parent directory and a filename, returns just the portion after the directory.
#[allow(unused)]
fn file_key(dir: impl AsRef<Path>, path: impl AsRef<Path>) -> String {
    let res_dir = dir.as_ref();
    let mut path = path.as_ref();
    let mut parts = vec![];

    while path != res_dir {
        {
            let path = path.file_name();
            if path.is_none() {
                break;
            }

            let path = path.unwrap();
            let path_str = path.to_str();
            if path_str.is_none() {
                break;
            }

            parts.push(path_str.unwrap().to_string());
        }
        path = path.parent().unwrap();
    }

    let mut key = String::new();
    for part in parts.iter().rev() {
        if !key.is_empty() {
            key.push('/');
        }

        key.push_str(part);
    }

    // Strip off the toml extension as needed
    let mut key = PathBuf::from(key);
    if is_toml(&key) {
        key = key.with_extension("");
    }

    key.to_str().unwrap().to_owned()
}

fn is_cargo_build() -> bool {
    var("CARGO").is_ok()
}

/// Returns `true` when a given path has the `.toml` file extension.
fn is_toml(path: impl AsRef<Path>) -> bool {
    path.as_ref()
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| *ext == "toml")
        .is_some()
}

/// Returns either the parent directory of the given path or the project root if the path has no
/// parent.
fn parent(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref()
        .parent()
        .map(|path| path.to_owned())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn parse_hex_color(val: &str) -> Option<[u8; 4]> {
    let mut res = [u8::MAX; 4];
    let len = val.len();
    match len {
        4 | 5 => {
            res[0] = u8::from_str_radix(&val[1..2].repeat(2), 16).unwrap();
            res[1] = u8::from_str_radix(&val[2..3].repeat(2), 16).unwrap();
            res[2] = u8::from_str_radix(&val[3..4].repeat(2), 16).unwrap();
        }
        7 | 9 => {
            res[0] = u8::from_str_radix(&val[1..3], 16).unwrap();
            res[1] = u8::from_str_radix(&val[3..5], 16).unwrap();
            res[2] = u8::from_str_radix(&val[5..7], 16).unwrap();
        }
        _ => return None,
    }

    res[3] = match len {
        5 => u8::from_str_radix(&val[4..5].repeat(2), 16).unwrap(),
        9 => u8::from_str_radix(&val[7..9], 16).unwrap(),
        _ => u8::MAX,
    };

    Some(res)
}

fn parse_hex_scalar(val: &str) -> Option<u8> {
    match val.len() {
        2 => Some(u8::from_str_radix(&val[1..2].repeat(2), 16).unwrap()),
        3 => Some(u8::from_str_radix(&val[1..3], 16).unwrap()),
        _ => None,
    }
}

fn re_run_if_changed(p: impl AsRef<Path>) {
    if is_cargo_build() {
        println!("cargo:rerun-if-changed={}", p.as_ref().display());
    }
}

/// Euler rotation sequences.
///
/// The angles are applied starting from the right. E.g. XYZ will first apply the z-axis rotation.
///
/// YXZ can be used for yaw (y-axis), pitch (x-axis), roll (z-axis).
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
pub enum Euler {
    /// Intrinsic three-axis rotation XYZ
    #[serde(rename = "xyz")]
    XYZ,
    /// Intrinsic three-axis rotation XZY
    #[serde(rename = "xzy")]
    XZY,
    /// Intrinsic three-axis rotation YXZ
    #[serde(rename = "yxz")]
    YXZ,
    /// Intrinsic three-axis rotation YZX
    #[serde(rename = "yzx")]
    YZX,
    /// Intrinsic three-axis rotation ZXY
    #[serde(rename = "zxy")]
    ZXY,
    /// Intrinsic three-axis rotation ZYX
    #[serde(rename = "zyx")]
    ZYX,
}

trait Canonicalize {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>);

    /// Gets the fully rooted source path.
    ///
    /// If `src` is relative, then `src_dir` is used to determine the relative parent.
    /// If `src` is absolute, then `project_dir` is considered to be its root.
    fn canonicalize_project_path(
        project_dir: impl AsRef<Path>,
        src_dir: impl AsRef<Path>,
        src: impl AsRef<Path>,
    ) -> PathBuf {
        //trace!("Getting path for {} in {} (res_dir={})", path.as_ref().display(), path_dir.as_ref().display(), res_dir.as_ref().display());

        // Absolute paths are 'project aka resource directory' absolute, not *your host file system*
        // absolute!
        if src.as_ref().is_absolute() {
            // TODO: This could be way simpler!

            // Build an array of path items (file and directories) until the root
            let mut temp = Some(src.as_ref());
            let mut parts = vec![];
            while let Some(path) = temp {
                if let Some(part) = path.file_name() {
                    parts.push(part);
                    temp = path.parent();
                } else {
                    break;
                }
            }

            // Paste the incoming path (minus root) onto the res_dir parameter
            let mut temp = project_dir.as_ref().to_path_buf();
            for part in parts.iter().rev() {
                temp = temp.join(part);
            }

            temp.canonicalize().unwrap_or_else(|_| {
                panic!(
                    "{} not found, unable to canonicalize absolute path using {} with {}",
                    temp.display(),
                    project_dir.as_ref().display(),
                    src.as_ref().display(),
                );
            })
        } else {
            let temp = src_dir.as_ref().join(&src);
            temp.canonicalize().unwrap_or_else(|_| {
                panic!(
                    "{} not found, unable to canonicalize relative path using {} with {}",
                    temp.display(),
                    src_dir.as_ref().display(),
                    src.as_ref().display(),
                );
            })
        }
    }
}

impl PakBuf {
    pub fn bake(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> anyhow::Result<()> {
        re_run_if_changed(&src);

        let rt = Arc::new(Runtime::new()?);
        let mut tasks = vec![];
        let writer = Arc::new(Mutex::new(Default::default()));

        // Load the source file into an Asset::Content instance
        let src_dir = parent(&src);
        let content = Asset::read(&src)?
            .into_content()
            .context("Unable to read asset file")?;

        // Process each file we find as a separate runtime task
        for asset_glob in content
            .groups()
            .into_iter()
            .filter(|group| group.enabled())
            .flat_map(|group| group.asset_globs())
        {
            let asset_paths = glob(src_dir.join(asset_glob).to_string_lossy().as_ref())
                .context("Unable to glob source directory")?;
            for asset_path in asset_paths {
                let asset_path = asset_path.context("Unable to get asset path")?;

                info!("processing {}", asset_path.display());

                re_run_if_changed(&asset_path);

                match asset_path
                    .extension()
                    .map(|ext| ext.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .to_lowercase()
                    .as_str()
                {
                    "glb" | "gltf" => {
                        // Note that direct references like this build a model, not an animation
                        // To build an animation you must specify a .toml file
                        let writer = Arc::clone(&writer);
                        let src_dir = src_dir.clone();
                        let asset_path = asset_path.clone();
                        tasks.push(rt.spawn_blocking(move || {
                            Model::new(&asset_path)
                                .bake(&writer, &src_dir, Some(&asset_path))
                                .unwrap();
                        }));
                    }
                    "jpg" | "jpeg" | "png" | "bmp" | "tga" | "dds" | "webp" | "gif" | "ico"
                    | "tiff" => {
                        let writer = Arc::clone(&writer);
                        let src_dir = src_dir.clone();
                        let asset_path = asset_path.clone();
                        tasks.push(rt.spawn_blocking(move || {
                            Bitmap::new(&asset_path)
                                .bake_from_path(&writer, src_dir, Some(asset_path))
                                .unwrap();
                        }));
                    }
                    "toml" => {
                        let asset = Asset::read(&asset_path)?;
                        let asset_parent = parent(&asset_path);

                        match asset {
                            Asset::Animation(mut anim) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    anim.canonicalize(&src_dir, &asset_parent);
                                    anim.bake(&writer, src_dir, asset_path).unwrap();
                                }));
                            }
                            Asset::Bitmap(mut bitmap) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    bitmap.canonicalize(&src_dir, &asset_parent);
                                    bitmap
                                        .bake_from_path(&writer, src_dir, Some(asset_path))
                                        .unwrap();
                                }));
                            }
                            Asset::BitmapFont(mut blob) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    blob.canonicalize(&src_dir, &asset_parent);
                                    blob.bake_bitmap_font(&writer, src_dir, asset_path).unwrap();
                                }));
                            }
                            Asset::Material(mut material) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                let rt2 = rt.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    material.canonicalize(&src_dir, &asset_parent);
                                    material
                                        .bake(
                                            &rt2,
                                            &writer,
                                            src_dir,
                                            asset_parent,
                                            Some(asset_path),
                                        )
                                        .unwrap();
                                }));
                            }
                            Asset::Model(mut model) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    model.canonicalize(&src_dir, &asset_parent);
                                    model.bake(&writer, &src_dir, Some(&asset_path)).unwrap();
                                }));
                            }
                            Asset::Scene(mut scene) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                let rt2 = rt.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    scene.canonicalize(&src_dir, &asset_parent);
                                    scene.bake(&rt2, &writer, &src_dir, &asset_path).unwrap();
                                }));
                            }
                            _ => unimplemented!(),
                        }
                    }
                    _ => {
                        let writer = Arc::clone(&writer);
                        let src_dir = src_dir.clone();
                        let asset_path = asset_path.clone();
                        tasks.push(rt.spawn_blocking(move || {
                            let blob = Blob { src: asset_path };
                            blob.bake(&writer, &src_dir).unwrap();
                        }));
                    }
                }
            }
        }

        rt.block_on(async move {
            for task in tasks.into_iter() {
                task.await.unwrap();
            }

            let dst = dst.as_ref().to_path_buf();
            if let Some(parent) = dst.parent() {
                create_dir_all(parent)
                    .unwrap_or_else(|_| panic!("Unable to create directory {}", parent.display()));
            }

            writer
                .lock()
                .write(&dst)
                .unwrap_or_else(|_| panic!("Unable to write pak file {}", dst.display()));
        });

        Ok(())
    }
}
