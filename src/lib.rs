#![allow(dead_code)]

mod anim;
mod bitmap;
mod bitmap_font;
mod compression;
mod index;
mod model;
mod scene;

#[cfg(feature = "bake")]
pub mod buf;

pub use self::{
    anim::{Animation, Channel, Interpolation, Outputs},
    bitmap::{Bitmap, BitmapColor, BitmapFormat},
    bitmap_font::BitmapFont,
    index::{IndexBuffer, IndexType},
    model::{Joint, Mesh, MeshPart, Model, Skin, VertexType},
    scene::{GeometryData, GeometryRef, ReferenceData, ReferenceRef, Scene},
};

use {
    self::compression::Compression,
    log::{trace, warn},
    paste::paste,
    serde::{de::DeserializeOwned, Deserialize, Serialize},
    std::{
        collections::HashMap,
        fmt::{Debug, Formatter},
        fs::File,
        io::{BufReader, Cursor, Error, ErrorKind, Read, Seek, SeekFrom},
        ops::Range,
        path::{Path, PathBuf},
    },
};

pub type Vec3 = [f32; 3];
pub type Quat = [f32; 4];
pub type Mat4 = [f32; 16];

#[derive(Debug, Default, Deserialize, Serialize)]
struct Data {
    // These fields are handled by bincode serialization as-is
    ids: HashMap<String, Id>,
    materials: Vec<MaterialInfo>,

    // These fields are loaded on demand
    anims: Vec<DataRef<Animation>>,
    bitmap_fonts: Vec<DataRef<BitmapFont>>,
    bitmaps: Vec<DataRef<Bitmap>>,
    blobs: Vec<DataRef<Vec<u8>>>,
    models: Vec<DataRef<Model>>,
    scenes: Vec<DataRef<Scene>>,
}

#[derive(Deserialize, PartialEq, Serialize)]
enum DataRef<T> {
    Data(T),
    Ref(Range<u32>),
}

impl<T> DataRef<T> {
    fn as_data(&self) -> Option<&T> {
        match self {
            Self::Data(ref t) => Some(t),
            _ => {
                warn!("Expected data but found position and length");

                None
            }
        }
    }

    fn pos_len(&self) -> Option<(u64, usize)> {
        match self {
            Self::Ref(range) => Some((range.start as _, (range.end - range.start) as _)),
            _ => {
                warn!("Expected position and length but found data");

                None
            }
        }
    }
}

impl<T> DataRef<T>
where
    T: Serialize,
{
    fn serialize(&self) -> Result<Vec<u8>, Error> {
        let mut buf = vec![];
        bincode::serialize_into(&mut buf, &self.as_data().unwrap())
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;

        Ok(buf)
    }
}

impl<T> Debug for DataRef<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Data(_) => "Data",
            Self::Ref(_) => "DataRef",
        })
    }
}

macro_rules! id_enum {
    ($($variant:ident),*) => {
        paste::paste! {
            #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
            enum Id {
                $(
                    $variant([<$variant Id>]),
                )*
            }

            impl Id {
                $(
                    fn [<as_ $variant:snake>](&self) -> Option<[<$variant Id>]> {
                        match self {
                            Self::$variant(id) => Some(*id),
                            _ => None,
                        }
                    }
                )*
            }

            $(
                impl From<[<$variant Id>]> for Id {
                    fn from(id: [<$variant Id>]) -> Self {
                        Self::$variant(id)
                    }
                }
            )*
        }
    };
}

id_enum!(Animation, Bitmap, BitmapFont, Blob, Material, Model, Scene);

macro_rules! id_struct {
    ($name: ident) => {
        paste! {
            #[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, PartialOrd, Ord,
                Serialize)]
            pub struct [<$name Id>](pub usize);
        }
    };
}

id_struct!(Animation);
id_struct!(Bitmap);
id_struct!(BitmapFont);
id_struct!(Blob);
id_struct!(Material);
id_struct!(Model);
id_struct!(Scene);

/// Holds bitmap handles to match what was setup in the asset `.toml` file.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct MaterialInfo {
    /// Three or four channel base color, aka albedo or diffuse, of the material.
    pub color: BitmapId,

    /// A standard three channel emissive color map.
    pub emissive: Option<BitmapId>,

    /// A standard three channel normal map.
    pub normal: BitmapId,

    /// A two channel bitmap of the metalness (red) and roughness (green) PBR parameters.
    ///
    /// Optionally has a third channel (blue) for displacement.
    pub params: BitmapId,
}

pub trait Pak {
    // --- "Get by id" functions

    /// Gets the pak-unique `AnimationId` corresponding to the given key, if one exsits.
    fn animation_id(&self, key: impl AsRef<str>) -> Option<AnimationId>;

    /// Gets the pak-unique `BitmapFontId` corresponding to the given key, if one exsits.
    fn bitmap_font_id(&self, key: impl AsRef<str>) -> Option<BitmapFontId>;

    /// Gets the pak-unique `BitmapId` corresponding to the given key, if one exsits.
    fn bitmap_id(&self, key: impl AsRef<str>) -> Option<BitmapId>;

    /// Gets the pak-unique `BlobId` corresponding to the given key, if one exsits.
    fn blob_id(&self, key: impl AsRef<str>) -> Option<BlobId>;

    /// Gets the pak-unique `MaterialId` corresponding to the given key, if one exsits.
    fn material_id(&self, key: impl AsRef<str>) -> Option<MaterialId>;

    /// Gets the pak-unique `ModelId` corresponding to the given key, if one exsits.
    fn model_id(&self, key: impl AsRef<str>) -> Option<ModelId>;

    /// Gets the pak-unique `SceneId` corresponding to the given key, if one exsits.
    fn scene_id(&self, key: impl AsRef<str>) -> Option<SceneId>;

    // --- "Read" functions

    /// Gets the corresponding animation for the given ID.
    fn read_animation_id(&mut self, id: impl Into<AnimationId>) -> Result<Animation, Error>;

    /// Reads the corresponding bitmap for the given ID.
    fn read_bitmap_font_id(&mut self, id: impl Into<BitmapFontId>) -> Result<BitmapFont, Error>;

    /// Reads the corresponding bitmap for the given ID.
    fn read_bitmap_id(&mut self, id: impl Into<BitmapId>) -> Result<Bitmap, Error>;

    /// Gets the corresponding blob for the given ID.
    fn read_blob_id(&mut self, id: impl Into<BlobId>) -> Result<Vec<u8>, Error>;

    /// Gets the material for the given handle, if one exsits.
    fn read_material_id(&self, id: impl Into<MaterialId>) -> Option<MaterialInfo>;

    /// Gets the corresponding animation for the given ID.
    fn read_model_id(&mut self, id: impl Into<ModelId>) -> Result<Model, Error>;

    /// Gets the corresponding animation for the given ID.
    fn read_scene_id(&mut self, id: impl Into<SceneId>) -> Result<Scene, Error>;

    // --- Convenience functions

    /// Gets the material corresponding to the given key, if one exsits.
    fn read_material(&self, key: impl AsRef<str>) -> Option<MaterialInfo> {
        trace!("Reading material {}", key.as_ref());

        if let Some(id) = self.material_id(key) {
            self.read_material_id(id)
        } else {
            None
        }
    }

    fn read_animation(&mut self, key: impl AsRef<str>) -> Result<Animation, Error> {
        trace!("Reading animation {}", key.as_ref());

        if let Some(h) = self.animation_id(key) {
            self.read_animation_id(h)
        } else {
            Err(Error::from(ErrorKind::InvalidInput))
        }
    }

    fn read_bitmap_font(&mut self, key: impl AsRef<str>) -> Result<BitmapFont, Error> {
        trace!("Reading bitmap font {}", key.as_ref());

        if let Some(h) = self.bitmap_font_id(key) {
            self.read_bitmap_font_id(h)
        } else {
            Err(Error::from(ErrorKind::InvalidInput))
        }
    }

    fn read_bitmap(&mut self, key: impl AsRef<str>) -> Result<Bitmap, Error> {
        trace!("Reading bitmap {}", key.as_ref());

        if let Some(h) = self.bitmap_id(key) {
            self.read_bitmap_id(h)
        } else {
            Err(Error::from(ErrorKind::InvalidInput))
        }
    }

    fn read_blob(&mut self, key: impl AsRef<str>) -> Result<Vec<u8>, Error> {
        trace!("Reading blob {}", key.as_ref());

        if let Some(h) = self.blob_id(key) {
            self.read_blob_id(h)
        } else {
            Err(Error::from(ErrorKind::InvalidInput))
        }
    }

    fn read_model(&mut self, key: impl AsRef<str>) -> Result<Model, Error> {
        trace!("Reading model {}", key.as_ref());

        if let Some(h) = self.model_id(key) {
            self.read_model_id(h)
        } else {
            Err(Error::from(ErrorKind::InvalidInput))
        }
    }

    fn read_scene(&mut self, key: impl AsRef<str>) -> Result<Scene, Error> {
        trace!("Reading scene {}", key.as_ref());

        if let Some(h) = self.scene_id(key) {
            self.read_scene_id(h)
        } else {
            Err(Error::from(ErrorKind::InvalidInput))
        }
    }
}

/// Main serialization container for the `.pak` file format.
#[derive(Debug)]
pub struct PakBuf {
    compression: Option<Compression>,
    data: Data,
    reader: Box<dyn Stream>,
}

impl PakBuf {
    pub fn animation_count(&self) -> usize {
        self.data.anims.len()
    }

    pub fn bitmap_count(&self) -> usize {
        self.data.bitmaps.len()
    }

    pub fn bitmap_font_count(&self) -> usize {
        self.data.bitmap_fonts.len()
    }

    pub fn blob_count(&self) -> usize {
        self.data.blobs.len()
    }

    fn deserialize<T>(&mut self, pos: u64, len: usize) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        trace!("Read data: {len} bytes ({pos}..{})", pos + len as u64);

        // Create a zero-filled buffer
        let mut buf = vec![0; len];

        // Read the data into our buffer
        self.reader.seek(SeekFrom::Start(pos))?;
        self.reader.read_exact(&mut buf)?;
        let data = buf.as_slice();

        // Optionally create a compression reader (or just use the one we have)
        if let Some(compressed) = self.compression {
            bincode::deserialize_from(compressed.new_reader(data))
        } else {
            bincode::deserialize_from(data)
        }
        .map_err(|err| {
            warn!("Unable to deserialize: {}", err);

            Error::from(ErrorKind::InvalidData)
        })
    }

    pub fn from_stream(mut stream: impl Stream + 'static) -> Result<Self, Error> {
        let magic_bytes: [u8; 20] = bincode::deserialize_from(&mut stream).map_err(|_| {
            warn!("Unable to read magic bytes");

            Error::from(ErrorKind::InvalidData)
        })?;

        if String::from_utf8(magic_bytes.into()).unwrap_or_default() != "ATTACKGOAT-PAK-V1.0 " {
            warn!("Unsupported magic bytes");

            return Err(Error::from(ErrorKind::InvalidData));
        }

        // Read the number of bytes we must 'skip' in order to read the main data
        let skip: u32 = bincode::deserialize_from(&mut stream).map_err(|_| {
            warn!("Unable to read skip length");

            Error::from(ErrorKind::InvalidData)
        })?;

        let compression: Option<Compression> =
            bincode::deserialize_from(&mut stream).map_err(|_| {
                warn!("Unable to read compression data");

                Error::from(ErrorKind::InvalidData)
            })?;

        // Read the compressed main data
        stream.seek(SeekFrom::Start(skip as _))?;
        let data: Data = {
            let mut compressed = if let Some(compressed) = compression {
                compressed.new_reader(&mut stream)
            } else {
                Box::new(&mut stream)
            };
            bincode::deserialize_from(&mut compressed).map_err(|_| {
                warn!("Unable to read header");

                Error::from(ErrorKind::InvalidData)
            })?
        };

        trace!(
            "Read header: {} bytes ({} keys)",
            stream.stream_position()? - skip as u64,
            data.ids.len()
        );

        Ok(Self {
            compression,
            data,
            reader: Box::new(stream),
        })
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.data.ids.keys().map(|key| key.as_str())
    }

    pub fn model_count(&self) -> usize {
        self.data.models.len()
    }

    pub fn material_count(&self) -> usize {
        self.data.materials.len()
    }

    /// Opens the given path and decodes a `Pak`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let buf = BufReader::new(file);

        Self::from_stream(PakFile { buf, path })
    }

    pub fn scene_count(&self) -> usize {
        self.data.scenes.len()
    }
}

impl Pak for PakBuf {
    /// Gets the pak-unique `AnimationId` corresponding to the given key, if one exsits.
    fn animation_id(&self, key: impl AsRef<str>) -> Option<AnimationId> {
        self.data
            .ids
            .get(key.as_ref())
            .and_then(|id| id.as_animation())
    }

    /// Gets the pak-unique `BitmapFontId` corresponding to the given key, if one exsits.
    fn bitmap_font_id(&self, key: impl AsRef<str>) -> Option<BitmapFontId> {
        self.data
            .ids
            .get(key.as_ref())
            .and_then(|id| id.as_bitmap_font())
    }

    /// Gets the pak-unique `BitmapId` corresponding to the given key, if one exsits.
    fn bitmap_id(&self, key: impl AsRef<str>) -> Option<BitmapId> {
        self.data
            .ids
            .get(key.as_ref())
            .and_then(|id| id.as_bitmap())
    }

    /// Gets the pak-unique `BlobId` corresponding to the given key, if one exsits.
    fn blob_id(&self, key: impl AsRef<str>) -> Option<BlobId> {
        self.data.ids.get(key.as_ref()).and_then(|id| id.as_blob())
    }

    /// Gets the pak-unique `MaterialId` corresponding to the given key, if one exsits.
    fn material_id(&self, key: impl AsRef<str>) -> Option<MaterialId> {
        self.data
            .ids
            .get(key.as_ref())
            .and_then(|id| id.as_material())
    }

    /// Gets the pak-unique `ModelId` corresponding to the given key, if one exsits.
    fn model_id(&self, key: impl AsRef<str>) -> Option<ModelId> {
        self.data.ids.get(key.as_ref()).and_then(|id| id.as_model())
    }

    /// Gets the pak-unique `SceneId` corresponding to the given key, if one exsits.
    fn scene_id(&self, key: impl AsRef<str>) -> Option<SceneId> {
        self.data.ids.get(key.as_ref()).and_then(|id| id.as_scene())
    }

    /// Gets the corresponding animation for the given ID.
    fn read_animation_id(&mut self, id: impl Into<AnimationId>) -> Result<Animation, Error> {
        let id = id.into();

        trace!("Deserializing animation {}", id.0);

        let (pos, len) = self.data.anims[id.0]
            .pos_len()
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
        self.deserialize(pos, len)
    }

    /// Reads the corresponding bitmap for the given ID.
    fn read_bitmap_font_id(&mut self, id: impl Into<BitmapFontId>) -> Result<BitmapFont, Error> {
        let id = id.into();

        trace!("Deserializing bitmap font {}", id.0);

        let (pos, len) = self.data.bitmap_fonts[id.0]
            .pos_len()
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
        self.deserialize(pos, len)
    }

    /// Reads the corresponding bitmap for the given ID.
    fn read_bitmap_id(&mut self, id: impl Into<BitmapId>) -> Result<Bitmap, Error> {
        let id = id.into();

        trace!("Deserializing bitmap {}", id.0);

        let (pos, len) = self.data.bitmaps[id.0]
            .pos_len()
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
        self.deserialize(pos, len)
    }

    /// Gets the corresponding blob for the given ID.
    fn read_blob_id(&mut self, id: impl Into<BlobId>) -> Result<Vec<u8>, Error> {
        let id = id.into();

        trace!("Deserializing blob {}", id.0);

        let (pos, len) = self.data.blobs[id.0]
            .pos_len()
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
        self.deserialize(pos, len)
    }

    /// Gets the material for the given ID.
    fn read_material_id(&self, id: impl Into<MaterialId>) -> Option<MaterialInfo> {
        let id = id.into();

        self.data.materials.get(id.0).copied()
    }

    /// Gets the corresponding animation for the given ID.
    fn read_model_id(&mut self, id: impl Into<ModelId>) -> Result<Model, Error> {
        let id = id.into();

        trace!("Deserializing model {}", id.0);

        let (pos, len) = self.data.models[id.0]
            .pos_len()
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
        self.deserialize(pos, len)
    }

    /// Gets the corresponding animation for the given ID.
    fn read_scene_id(&mut self, id: impl Into<SceneId>) -> Result<Scene, Error> {
        let id = id.into();

        trace!("Deserializing scene {}", id.0);

        let (pos, len) = self.data.scenes[id.0]
            .pos_len()
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
        self.deserialize(pos, len)
    }
}

#[derive(Debug)]
struct PakFile {
    buf: BufReader<File>,
    path: PathBuf,
}

impl From<&'static [u8]> for PakBuf {
    fn from(data: &'static [u8]) -> Self {
        // This is infalliable for the given input so unwrap is aok
        Self::from_stream(Cursor::new(data)).unwrap()
    }
}

pub trait Stream: Debug + Read + Seek + Send {
    fn open(&self) -> Result<Box<dyn Stream>, Error>;
}

impl Stream for PakFile {
    fn open(&self) -> Result<Box<dyn Stream>, Error> {
        let file = File::open(&self.path)?;
        let buf = BufReader::new(file);

        Ok(Box::new(PakFile {
            buf,
            path: self.path.clone(),
        }))
    }
}

impl Read for PakFile {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.buf.read(buf)
    }
}

impl Seek for PakFile {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        self.buf.seek(pos)
    }
}

impl Stream for Cursor<&'static [u8]> {
    fn open(&self) -> Result<Box<dyn Stream>, Error> {
        Ok(Box::new(Cursor::new(*self.get_ref())))
    }
}
