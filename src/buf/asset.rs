use {
    super::{
        anim::AnimationAsset,
        bitmap::BitmapAsset,
        blob::BlobAsset,
        content::Content,
        material::{MaterialAsset, MaterialParams},
        mesh::MeshAsset,
        scene::SceneAsset,
    },
    anyhow::{Context, bail},
    ordered_float::OrderedFloat,
    serde::Deserialize,
    std::{
        fs::{exists, read_to_string},
        io::{Error, ErrorKind},
        path::Path,
    },
};

/// A collection type containing all supported asset file types.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub enum Asset {
    /// `.glb` or `.gltf` mesh animations.
    Animation(AnimationAsset),
    /// `.jpeg` and other regular images.
    Bitmap(BitmapAsset),
    /// `.fnt` bitmapped fonts.
    BitmapFont(BlobAsset),
    /// Raw byte blobs.
    Blob(BlobAsset),
    /// Solid color.
    ColorRgb([OrderedFloat<f32>; 3]),
    /// Solid color with alpha channel.
    ColorRgba([OrderedFloat<f32>; 4]),
    /// Top-level content files which simply group other asset files for ease of use.
    Content(Content),
    /// Used for 3D mesh rendering.
    Material(MaterialAsset),
    /// Used to cache the params texture during material baking.
    MaterialParams(MaterialParams),
    /// `.glb` or `.gltf` 3D meshes.
    Mesh(MeshAsset),
    /// Describes position/orientation/scale and tagged data specific to each program.
    ///
    /// You are expected to write some manner of and export tool in order to create this file type
    /// using an external editor.
    Scene(SceneAsset),
}

impl Asset {
    /// Reads an asset file from disk.
    pub fn read(filename: impl AsRef<Path>) -> anyhow::Result<Self> {
        let str = read_to_string(&filename).context("Reading asset file as a string")?;
        let val: Schema = toml::from_str(&str).context("Parsing asset toml")?;
        let res = if let Some(mut anim) = val.anim {
            // If the source was not set, infer it from the toml filename
            if anim.src().is_none() {
                for ext in ["glb", "gltf"] {
                    let src = filename.as_ref().with_extension(ext);
                    if let Ok(true) = exists(&src) {
                        // Source is just the filename; it is relative to the toml being read
                        anim.set_src(src.file_name().unwrap_or_default());
                        break;
                    }
                }
            }

            Self::Animation(anim)
        } else if let Some(mut bitmap) = val.bitmap {
            // If the source was not set, infer it from the toml filename
            if bitmap.src().is_none() {
                for ext in [
                    "jpg", "jpeg", "png", "bmp", "tga", "dds", "webp", "gif", "ico", "tiff",
                ] {
                    let src = filename.as_ref().with_extension(ext);
                    if let Ok(true) = exists(&src) {
                        // Source is just the filename; it is relative to the toml being read
                        bitmap.set_src(src.file_name().unwrap_or_default());
                        break;
                    }
                }
            }

            Self::Bitmap(bitmap)
        } else if let Some(mut blob) = val.bitmap_font {
            // If the source was not set, infer it from the toml filename
            if blob.src().is_none() {
                for ext in ["fon", "fnt"] {
                    let src = filename.as_ref().with_extension(ext);
                    if let Ok(true) = exists(&src) {
                        // Source is just the filename; it is relative to the toml being read
                        blob.set_src(src.file_name().unwrap_or_default());
                        break;
                    }
                }
            }

            Self::BitmapFont(blob)
        } else if let Some(content) = val.content {
            Self::Content(content)
        } else if let Some(material) = val.material {
            Self::Material(material)
        } else if let Some(mut mesh) = val.mesh {
            // If the source was not set, infer it from the toml filename
            if mesh.src().is_none() {
                for ext in ["glb", "gltf"] {
                    let src = filename.as_ref().with_extension(ext);
                    if let Ok(true) = exists(&src) {
                        // Source is just the filename; it is relative to the toml being read
                        mesh.set_src(src.file_name().unwrap_or_default());
                        break;
                    }
                }
            }

            Self::Mesh(mesh)
        } else if let Some(scene) = val.scene {
            Self::Scene(scene)
        } else {
            bail!(Error::from(ErrorKind::InvalidData));
        };

        Ok(res)
    }

    /// Attempts to extract a `Bitmap` asset from this collection type.
    pub fn into_bitmap(self) -> Option<BitmapAsset> {
        match self {
            Self::Bitmap(bitmap) => Some(bitmap),
            _ => None,
        }
    }

    /// Attempts to extract a `Content` asset from this collection type.
    pub fn into_content(self) -> Option<Content> {
        match self {
            Self::Content(content) => Some(content),
            _ => None,
        }
    }

    /// Attempts to extract a `Material` asset from this collection type.
    pub fn into_material(self) -> Option<MaterialAsset> {
        match self {
            Self::Material(material) => Some(material),
            _ => None,
        }
    }

    /// Attempts to extract a `Mesh` asset from this collection type.
    pub fn into_mesh(self) -> Option<MeshAsset> {
        match self {
            Self::Mesh(mesh) => Some(mesh),
            _ => None,
        }
    }
}

impl From<BitmapAsset> for Asset {
    fn from(val: BitmapAsset) -> Self {
        Self::Bitmap(val)
    }
}

impl From<BlobAsset> for Asset {
    fn from(val: BlobAsset) -> Self {
        Self::Blob(val)
    }
}

impl From<[OrderedFloat<f32>; 3]> for Asset {
    fn from(val: [OrderedFloat<f32>; 3]) -> Self {
        Self::ColorRgb(val)
    }
}

impl From<[OrderedFloat<f32>; 4]> for Asset {
    fn from(val: [OrderedFloat<f32>; 4]) -> Self {
        Self::ColorRgba(val)
    }
}

impl From<MeshAsset> for Asset {
    fn from(val: MeshAsset) -> Self {
        Self::Mesh(val)
    }
}

impl From<MaterialAsset> for Asset {
    fn from(val: MaterialAsset) -> Self {
        Self::Material(val)
    }
}

impl From<SceneAsset> for Asset {
    fn from(val: SceneAsset) -> Self {
        Self::Scene(val)
    }
}

#[derive(Deserialize)]
struct Schema {
    #[serde(rename = "animation")]
    #[allow(unused)]
    anim: Option<AnimationAsset>,

    #[allow(unused)]
    bitmap: Option<BitmapAsset>,

    #[serde(rename = "bitmap-font")]
    #[allow(unused)]
    bitmap_font: Option<BlobAsset>,

    #[allow(unused)]
    content: Option<Content>,
    #[allow(unused)]
    material: Option<MaterialAsset>,
    #[allow(unused)]
    mesh: Option<MeshAsset>,
    #[allow(unused)]
    scene: Option<SceneAsset>,
}
