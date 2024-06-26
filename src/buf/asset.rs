use {
    super::{
        anim::AnimationAsset,
        bitmap::BitmapAsset,
        blob::BlobAsset,
        content::Content,
        material::{MaterialAsset, MaterialParams},
        model::ModelAsset,
        scene::SceneAsset,
    },
    anyhow::{bail, Context},
    serde::Deserialize,
    std::{
        fs::read_to_string,
        io::{Error, ErrorKind},
        path::Path,
    },
};

/// A collection type containing all supported asset file types.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub enum Asset {
    /// `.glb` or `.gltf` model animations.
    Animation(AnimationAsset),
    /// `.jpeg` and other regular images.
    Bitmap(BitmapAsset),
    /// `.fnt` bitmapped fonts.
    BitmapFont(BlobAsset),
    /// Raw byte blobs.
    Blob(BlobAsset),
    /// Solid color.
    ColorRgb([u8; 3]),
    /// Solid color with alpha channel.
    ColorRgba([u8; 4]),
    /// Top-level content files which simply group other asset files for ease of use.
    Content(Content),
    /// Used for 3D model rendering.
    Material(MaterialAsset),
    /// Used to cache the params texture during material baking.
    MaterialParams(MaterialParams),
    /// `.glb` or `.gltf` 3D models.
    Model(ModelAsset),
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
        let res = if let Some(val) = val.anim {
            Self::Animation(val)
        } else if let Some(val) = val.bitmap {
            Self::Bitmap(val)
        } else if let Some(val) = val.bitmap_font {
            Self::BitmapFont(val)
        } else if let Some(val) = val.content {
            Self::Content(val)
        } else if let Some(val) = val.material {
            Self::Material(val)
        } else if let Some(val) = val.model {
            Self::Model(val)
        } else if let Some(val) = val.scene {
            Self::Scene(val)
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

    /// Attempts to extract a `Model` asset from this collection type.
    pub fn into_model(self) -> Option<ModelAsset> {
        match self {
            Self::Model(model) => Some(model),
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

impl From<[u8; 3]> for Asset {
    fn from(val: [u8; 3]) -> Self {
        Self::ColorRgb(val)
    }
}

impl From<[u8; 4]> for Asset {
    fn from(val: [u8; 4]) -> Self {
        Self::ColorRgba(val)
    }
}

impl From<ModelAsset> for Asset {
    fn from(val: ModelAsset) -> Self {
        Self::Model(val)
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
    model: Option<ModelAsset>,
    #[allow(unused)]
    scene: Option<SceneAsset>,
}
