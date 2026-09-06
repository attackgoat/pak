pub mod anim;
pub mod bitmap;
pub mod bitmap_font;
pub mod index;
pub mod mesh;
pub mod opacity_micromap;
pub mod scene;

#[cfg(feature = "bake")]
pub mod buf;

mod compression;

use {
    self::{
        anim::Animation,
        bitmap::{Bitmap, BitmapCompression, BitmapInfo, CompressedBitmap},
        bitmap_font::BitmapFont,
        compression::Compression,
        mesh::Mesh,
        opacity_micromap::{OpacityMicromap, OpacityMicromapInfo, OpacityMicromapKey},
        scene::Scene,
    },
    bitflags::bitflags,
    log::{trace, warn},
    paste::paste,
    serde::{Deserialize, Serialize, de::DeserializeOwned},
    std::{
        collections::BTreeMap,
        fmt::{Debug, Formatter},
        fs::File,
        io::{BufReader, Cursor, Error, ErrorKind, Read, Seek, SeekFrom},
        marker::PhantomData,
        mem::size_of,
        ops::Range,
        path::Path,
    },
};

pub type Vec3 = [f32; 3];
pub type Quat = [f32; 4];
pub type Mat4 = [f32; 16];

pub(crate) const PAK_HASH_LEN: usize = size_of::<u64>();
pub const MAX_STORED_PAYLOAD_BYTES: u64 = 1024 * 1024 * 1024;

const FNV_OFFSET: u64 = 0xcbf29ce484222325;
const FNV_PRIME: u64 = 0x100000001b3;

fn update_hash(hash: u64, data: &[u8]) -> u64 {
    data.iter().fold(hash, |hash, byte| {
        (hash ^ u64::from(*byte)).wrapping_mul(FNV_PRIME)
    })
}

pub(crate) fn pak_hash_stream(reader: &mut impl Read, len: u64) -> Result<u64, Error> {
    let mut hash = FNV_OFFSET;
    let mut remaining = len;
    let mut buf = [0; 8192];

    while remaining > 0 {
        let limit = if remaining < buf.len() as u64 {
            remaining as usize
        } else {
            buf.len()
        };
        let read = reader.read(&mut buf[..limit])?;
        if read == 0 {
            return Err(Error::from(ErrorKind::UnexpectedEof));
        }

        hash = update_hash(hash, &buf[..read]);
        remaining -= read as u64;
    }

    Ok(hash)
}

fn read_hash_trailer(reader: &mut impl Read) -> Result<u64, Error> {
    let mut hash = [0; PAK_HASH_LEN];
    reader.read_exact(&mut hash)?;
    let (hash, consumed) =
        bincode::serde::decode_from_slice::<u64, _>(&hash, bincode::config::legacy())
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;

    if consumed == PAK_HASH_LEN {
        Ok(hash)
    } else {
        Err(Error::from(ErrorKind::InvalidData))
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
struct Data {
    // These fields are handled by bincode serialization as-is
    generation: [u8; 32],
    segments: Vec<SegmentMeta>,
    ids: BTreeMap<String, Id>,
    materials: Vec<MaterialInfo>,
    opacity_micromap_infos: Vec<OpacityMicromapInfo>,

    // These fields are loaded on demand
    anims: Vec<DataRef<Animation>>,
    bitmap_fonts: Vec<DataRef<BitmapFont>>,
    bitmaps: Vec<BitmapData>,
    blobs: Vec<DataRef<Vec<u8>>>,
    meshes: Vec<DataRef<Mesh>>,
    opacity_micromaps: Vec<DataRef<OpacityMicromap>>,
    scenes: Vec<DataRef<Scene>>,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
struct SegmentMeta {
    name: String,
    filename: String,
    generation: [u8; 32],
    hash: u64,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct BitmapData {
    info: BitmapInfo,
    raw: DataRef<Bitmap>,
    compressed: Option<DataRef<CompressedBitmap>>,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct DataRange {
    segment: u16,
    range: Range<u64>,
    compression: Option<Compression>,
}

#[derive(Deserialize, PartialEq, Serialize)]
enum DataRef<T> {
    Ref(DataRange),
    #[serde(skip)]
    #[allow(dead_code)]
    Marker(PhantomData<T>),
}

impl<T> Clone for DataRef<T> {
    fn clone(&self) -> Self {
        match self {
            Self::Ref(range) => Self::Ref(range.clone()),
            Self::Marker(_) => Self::Marker(PhantomData),
        }
    }
}

impl<T> DataRef<T> {
    fn data_range(&self) -> Result<DataRange, Error> {
        match self {
            Self::Ref(data_ref) if data_ref.range.end >= data_ref.range.start => {
                Ok(data_ref.clone())
            }
            Self::Ref(_) => Err(Error::from(ErrorKind::InvalidData)),
            Self::Marker(_) => Err(Error::from(ErrorKind::InvalidInput)),
        }
    }
}

fn valid_segment_name(name: &str) -> bool {
    !name.is_empty()
        && name != "."
        && name != ".."
        && name.len() <= 64
        && name.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

impl<T> Debug for DataRef<T> {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str("DataRef")
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

id_enum!(Animation, Bitmap, BitmapFont, Blob, Material, Mesh, Scene);

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
id_struct!(Mesh);
id_struct!(OpacityMicromap);
id_struct!(Scene);

/// Holds bitmap handles to match what was setup in the asset `.toml` file.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct MaterialInfo {
    /// Whether base-color alpha should reject fragments below the engine cutoff.
    pub alpha_test: bool,

    /// Three or four channel base color, aka albedo or diffuse, of the material.
    pub color: BitmapId,

    /// A standard three channel emissive color map.
    pub emissive: Option<BitmapId>,

    /// A standard three channel normal map.
    pub normal: Option<BitmapId>,

    /// Optional RGBA material parameter map: metal, rough, height or occlusion, transmission.
    pub params: Option<BitmapId>,

    /// Compact authored material properties and parameter-channel usage.
    pub params_used: MaterialParameterFlags,

    /// Application-defined material data, using the same value types as scene data.
    pub data: scene::DataMap,
}

impl MaterialInfo {
    /// Returns the data for the given key, if it exists.
    pub fn data(&self, key: &str) -> Option<scene::DataRef<'_>> {
        self.data.get(key)
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    pub struct MaterialParameterFlags: u8 {
        const METAL = 1 << 0;
        const ROUGH = 1 << 1;
        const HEIGHT = 1 << 2;
        const TRANSMISSION = 1 << 3;
        const OCCLUSION = 1 << 4;
        const LANDSCAPE = 1 << 6;
    }
}

pub trait Pak {
    // --- "Get by id" functions

    /// Gets the pak-unique `AnimationId` corresponding to the given key, if one exists.
    fn animation_id(&self, key: impl AsRef<str>) -> Option<AnimationId>;

    /// Gets the pak-unique `BitmapFontId` corresponding to the given key, if one exists.
    fn bitmap_font_id(&self, key: impl AsRef<str>) -> Option<BitmapFontId>;

    /// Gets the pak-unique `BitmapId` corresponding to the given key, if one exists.
    fn bitmap_id(&self, key: impl AsRef<str>) -> Option<BitmapId>;

    /// Gets the pak-unique `BlobId` corresponding to the given key, if one exists.
    fn blob_id(&self, key: impl AsRef<str>) -> Option<BlobId>;

    /// Gets the pak-unique `MaterialId` corresponding to the given key, if one exists.
    fn material_id(&self, key: impl AsRef<str>) -> Option<MaterialId>;

    /// Gets the pak-unique `MeshId` corresponding to the given key, if one exists.
    fn mesh_id(&self, key: impl AsRef<str>) -> Option<MeshId>;

    /// Gets the pak-unique `SceneId` corresponding to the given key, if one exists.
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

    /// Opens a seekable reader over the corresponding blob payload.
    ///
    /// This is experimental and only supports blobs without outer payload compression. The reader
    /// skips the bincode length prefix and exposes only the byte payload.
    fn stream_blob_id(&self, id: impl Into<BlobId>) -> Result<BlobStream, Error>;

    /// Gets the material for the given handle, if one exists.
    fn read_material_id(&self, id: impl Into<MaterialId>) -> Option<MaterialInfo>;

    /// Gets the corresponding mesh for the given ID.
    fn read_mesh_id(&mut self, id: impl Into<MeshId>) -> Result<Mesh, Error>;

    /// Gets the corresponding scene for the given ID.
    fn read_scene_id(&mut self, id: impl Into<SceneId>) -> Result<Scene, Error>;

    // --- Convenience functions

    /// Gets the material corresponding to the given key, if one exists.
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

    fn stream_blob(&self, key: impl AsRef<str>) -> Result<BlobStream, Error> {
        trace!("Streaming blob {}", key.as_ref());

        if let Some(h) = self.blob_id(key) {
            self.stream_blob_id(h)
        } else {
            Err(Error::from(ErrorKind::InvalidInput))
        }
    }

    fn read_mesh(&mut self, key: impl AsRef<str>) -> Result<Mesh, Error> {
        trace!("Reading mesh {}", key.as_ref());

        if let Some(h) = self.mesh_id(key) {
            self.read_mesh_id(h)
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
    data: Data,
    readers: Vec<SegmentReader>,
}

#[derive(Debug)]
struct SegmentReader {
    stream: Box<dyn Stream>,
    range_base: u64,
    payload_start: u64,
    payload_end: u64,
    expected_hash: Option<u64>,
}

impl PakBuf {
    /// Returns the external payload filenames required beside the root pak.
    pub fn segment_file_names(&self) -> impl Iterator<Item = &str> {
        self.data
            .segments
            .iter()
            .map(|segment| segment.filename.as_str())
    }

    pub fn animation_count(&self) -> usize {
        self.data.anims.len()
    }

    pub fn bitmap_count(&self) -> usize {
        self.data.bitmaps.len()
    }

    pub fn opacity_micromap_count(&self) -> usize {
        self.data.opacity_micromaps.len()
    }

    pub fn opacity_micromap_infos(&self) -> &[OpacityMicromapInfo] {
        &self.data.opacity_micromap_infos
    }

    pub fn opacity_micromap_id(&self, key: OpacityMicromapKey) -> Option<OpacityMicromapId> {
        self.data
            .opacity_micromap_infos
            .binary_search_by_key(&key, |info| info.key)
            .ok()
            .map(|index| self.data.opacity_micromap_infos[index].payload)
    }

    pub fn read_opacity_micromap_id(
        &mut self,
        id: impl Into<OpacityMicromapId>,
    ) -> Result<OpacityMicromap, Error> {
        let id = id.into();
        let range = self
            .data
            .opacity_micromaps
            .get(id.0)
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?
            .data_range()?;
        let payload: OpacityMicromap = self.deserialize(&range)?;
        payload
            .validate()
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;

        Ok(payload)
    }

    pub fn read_opacity_micromap(
        &mut self,
        key: OpacityMicromapKey,
    ) -> Result<Option<OpacityMicromap>, Error> {
        self.opacity_micromap_id(key)
            .map(|id| self.read_opacity_micromap_id(id))
            .transpose()
    }

    /// Returns header-resident bitmap information without reading either payload variant.
    pub fn bitmap_info_id(&self, id: impl Into<BitmapId>) -> Option<BitmapInfo> {
        self.data.bitmaps.get(id.into().0).map(|bitmap| bitmap.info)
    }

    /// Returns header-resident bitmap information for a key without reading payload data.
    pub fn bitmap_info(&self, key: impl AsRef<str>) -> Option<BitmapInfo> {
        self.bitmap_id(key).and_then(|id| self.bitmap_info_id(id))
    }

    /// Reads only the optional block-compressed payload for a bitmap.
    pub fn read_compressed_bitmap_id(
        &mut self,
        id: impl Into<BitmapId>,
    ) -> Result<Option<CompressedBitmap>, Error> {
        let id = id.into();
        let (info, range) = {
            let bitmap = self
                .data
                .bitmaps
                .get(id.0)
                .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
            let Some(compressed) = &bitmap.compressed else {
                return if bitmap.info.has_compressed() {
                    Err(Error::from(ErrorKind::InvalidData))
                } else {
                    Ok(None)
                };
            };
            (bitmap.info, compressed.data_range()?)
        };

        trace!("Deserializing compressed bitmap {}", id.0);
        let compressed: CompressedBitmap = self.deserialize(&range)?;
        if info.compression() != Some(compressed.format())
            || compressed
                .validate(info.width(), info.height(), info.mip_levels())
                .is_err()
        {
            return Err(Error::from(ErrorKind::InvalidData));
        }

        Ok(Some(compressed))
    }

    /// Reads only the optional block-compressed payload for a bitmap key.
    pub fn read_compressed_bitmap(
        &mut self,
        key: impl AsRef<str>,
    ) -> Result<Option<CompressedBitmap>, Error> {
        let id = self
            .bitmap_id(key)
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
        self.read_compressed_bitmap_id(id)
    }

    pub fn bitmap_font_count(&self) -> usize {
        self.data.bitmap_fonts.len()
    }

    pub fn blob_count(&self) -> usize {
        self.data.blobs.len()
    }

    fn deserialize<T>(&mut self, range: &DataRange) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        let len_u64 = range
            .range
            .end
            .checked_sub(range.range.start)
            .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
        if len_u64 > MAX_STORED_PAYLOAD_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "stored pak payload exceeds allocation limit",
            ));
        }
        let len: usize = len_u64
            .try_into()
            .map_err(|_| Error::new(ErrorKind::InvalidData, "pak payload is too large"))?;
        let segment = self
            .readers
            .get_mut(range.segment as usize)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "unknown pak segment ID"))?;
        let pos = segment
            .range_base
            .checked_add(range.range.start)
            .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
        let end = segment
            .range_base
            .checked_add(range.range.end)
            .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
        if pos < segment.payload_start || end > segment.payload_end {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "pak range lies outside segment payload bounds",
            ));
        }
        trace!("Read segment {} data: {len} bytes", range.segment);

        // Create a zero-filled buffer
        let mut buf = Vec::new();
        buf.try_reserve_exact(len).map_err(|_| {
            Error::new(
                ErrorKind::OutOfMemory,
                "unable to allocate stored pak payload buffer",
            )
        })?;
        buf.resize(len, 0);

        // Read the data into our buffer
        segment.stream.seek(SeekFrom::Start(pos))?;
        segment.stream.read_exact(&mut buf)?;
        let data = buf.as_slice();

        // Optionally create a compression reader (or just use the one we have)
        if let Some(compressed) = range.compression {
            let mut reader = compressed.new_reader(data);
            let decoded =
                bincode::serde::decode_from_std_read(&mut reader, bincode::config::legacy())
                    .map_err(|err| {
                        warn!("Unable to deserialize: {}", err);

                        Error::from(ErrorKind::InvalidData)
                    })?;

            let mut trailing = [0; 1];
            match reader.read(&mut trailing) {
                Ok(0) => Ok(decoded),
                Ok(_) => {
                    warn!("Trailing bytes after deserialized data");

                    Err(Error::from(ErrorKind::InvalidData))
                }
                Err(err) => {
                    warn!("Unable to verify deserialized data end: {}", err);

                    Err(Error::from(ErrorKind::InvalidData))
                }
            }
        } else {
            let (decoded, consumed) =
                bincode::serde::decode_from_slice(data, bincode::config::legacy()).map_err(
                    |err| {
                        warn!("Unable to deserialize: {}", err);

                        Error::from(ErrorKind::InvalidData)
                    },
                )?;

            if consumed == data.len() {
                Ok(decoded)
            } else {
                warn!("Trailing bytes after deserialized data");

                Err(Error::from(ErrorKind::InvalidData))
            }
        }
    }

    fn read_root(
        mut stream: impl Stream + 'static,
    ) -> Result<(Data, Box<dyn Stream>, u64, u64), Error> {
        fn decode<T>(stream: &mut impl Read, msg: &str) -> Result<T, Error>
        where
            T: DeserializeOwned,
        {
            bincode::serde::decode_from_std_read(stream, bincode::config::legacy()).map_err(|_| {
                warn!("{}", msg);
                Error::from(ErrorKind::InvalidData)
            })
        }

        let magic_bytes: [u8; 20] = decode(&mut stream, "Unable to read magic bytes")?;
        if &magic_bytes != b"ATTACKGOAT-PAK-V1.8 " {
            warn!("Unsupported magic bytes");

            return Err(Error::from(ErrorKind::InvalidData));
        }

        // Read the number of bytes we must 'skip' in order to read the main data
        let skip: u64 = decode(&mut stream, "Unable to read skip length")?;

        let compression: Option<Compression> =
            decode(&mut stream, "Unable to read compression data")?;
        let payload_start = stream.stream_position()?;
        if skip < payload_start {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "pak index offset precedes payload prefix",
            ));
        }

        // Read the main data, excluding the hash trailer. The trailer is not validated here.
        let stream_end = stream.seek(SeekFrom::End(0))?;
        let header_end = stream_end
            .checked_sub(PAK_HASH_LEN as u64)
            .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
        let header_len = header_end
            .checked_sub(skip)
            .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
        stream.seek(SeekFrom::Start(skip))?;

        let data: Data = if let Some(compressed) = compression {
            let mut header = (&mut stream).take(header_len);
            let mut compressed = compressed.new_reader(&mut header);
            decode(&mut compressed, "Unable to read header")?
        } else {
            let mut header = (&mut stream).take(header_len);
            decode(&mut header, "Unable to read header")?
        };

        Self::validate_header(&data)?;

        trace!(
            "Read header: {} bytes ({} keys)",
            header_len,
            data.ids.len()
        );

        Ok((data, Box::new(stream), payload_start, skip))
    }

    fn validate_header(data: &Data) -> Result<(), Error> {
        if data
            .opacity_micromap_infos
            .windows(2)
            .any(|infos| infos[0].key >= infos[1].key)
        {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "opacity micromap keys are not strictly sorted",
            ));
        }

        for info in &data.opacity_micromap_infos {
            let material = data
                .materials
                .get(info.key.material.0)
                .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
            let bitmap = data
                .bitmaps
                .get(material.color.0)
                .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
            if info.key.mesh.0 >= data.meshes.len()
                || info.payload.0 >= data.opacity_micromaps.len()
                || info.key.source_mip >= bitmap.info.mip_levels()
                || bitmap.info.compression() != Some(BitmapCompression::Bc3)
            {
                return Err(Error::from(ErrorKind::InvalidData));
            }
        }

        Ok(())
    }

    pub fn from_stream(stream: impl Stream + 'static) -> Result<Self, Error> {
        let (data, stream, payload_start, payload_end) = Self::read_root(stream)?;
        if !data.segments.is_empty() {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "pak declares external segments; use PakBuf::open",
            ));
        }
        Ok(Self {
            data,
            readers: vec![SegmentReader {
                stream,
                range_base: 0,
                payload_start,
                payload_end,
                expected_hash: None,
            }],
        })
    }

    pub fn keys(&self) -> impl Iterator<Item = &str> {
        self.data.ids.keys().map(|key| key.as_str())
    }

    pub fn validate_hash(&self) -> Result<bool, Error> {
        for segment in &self.readers {
            if !Self::segment_hash_is_valid(segment)? {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn segment_hash_is_valid(segment: &SegmentReader) -> Result<bool, Error> {
        let mut reader = segment.stream.open()?;
        let stream_end = reader.seek(SeekFrom::End(0))?;
        let payload_len = stream_end
            .checked_sub(PAK_HASH_LEN as u64)
            .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
        reader.seek(SeekFrom::Start(0))?;
        let actual = pak_hash_stream(&mut reader, payload_len)?;
        let expected = read_hash_trailer(&mut reader)?;
        Ok(actual == expected
            && segment
                .expected_hash
                .is_none_or(|expected_hash| expected_hash == actual))
    }

    pub fn mesh_count(&self) -> usize {
        self.data.meshes.len()
    }

    pub fn material_count(&self) -> usize {
        self.data.materials.len()
    }

    /// Opens the given path and decodes a `Pak`.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, Error> {
        let path = path.as_ref().to_path_buf();
        let file = File::open(&path)?;
        let buf = BufReader::new(file);
        let (data, root, root_payload_start, root_payload_end) = Self::read_root(PakFile { buf })?;
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut readers = vec![SegmentReader {
            stream: root,
            range_base: 0,
            payload_start: root_payload_start,
            payload_end: root_payload_end,
            expected_hash: None,
        }];
        for (index, segment) in data.segments.iter().enumerate() {
            let mut components = Path::new(&segment.filename).components();
            if !valid_segment_name(&segment.name)
                || !matches!(components.next(), Some(std::path::Component::Normal(_)))
                || components.next().is_some()
            {
                return Err(Error::new(
                    ErrorKind::InvalidData,
                    "invalid pak segment metadata",
                ));
            }
            let segment_path = parent.join(&segment.filename);
            let file = File::open(&segment_path).map_err(|error| {
                Error::new(
                    error.kind(),
                    format!("unable to open pak segment {}: {error}", segment.name),
                )
            })?;
            let mut stream = PakFile {
                buf: BufReader::new(file),
            };
            let magic: [u8; 20] =
                bincode::serde::decode_from_std_read(&mut stream, bincode::config::legacy())
                    .map_err(|_| {
                        Error::new(ErrorKind::InvalidData, "truncated pak segment header")
                    })?;
            let id: u16 =
                bincode::serde::decode_from_std_read(&mut stream, bincode::config::legacy())
                    .map_err(|_| {
                        Error::new(ErrorKind::InvalidData, "truncated pak segment header")
                    })?;
            let name: String =
                bincode::serde::decode_from_std_read(&mut stream, bincode::config::legacy())
                    .map_err(|_| {
                        Error::new(ErrorKind::InvalidData, "truncated pak segment header")
                    })?;
            let generation: [u8; 32] =
                bincode::serde::decode_from_std_read(&mut stream, bincode::config::legacy())
                    .map_err(|_| {
                        Error::new(ErrorKind::InvalidData, "truncated pak segment header")
                    })?;
            if &magic != b"ATTACKGOAT-SEG-V1.2 "
                || id as usize != index + 1
                || name != segment.name
                || generation != segment.generation
            {
                return Err(Error::new(ErrorKind::InvalidData, "wrong pak segment file"));
            }
            let payload_start = stream.stream_position()?;
            let length = stream.seek(SeekFrom::End(0))?;
            if length < payload_start + PAK_HASH_LEN as u64 {
                return Err(Error::new(ErrorKind::InvalidData, "truncated pak segment"));
            }
            readers.push(SegmentReader {
                stream: Box::new(stream),
                range_base: payload_start,
                payload_start,
                payload_end: length - PAK_HASH_LEN as u64,
                expected_hash: Some(segment.hash),
            });
        }
        Ok(Self { data, readers })
    }

    pub fn scene_count(&self) -> usize {
        self.data.scenes.len()
    }
}

impl Pak for PakBuf {
    /// Gets the pak-unique `AnimationId` corresponding to the given key, if one exists.
    fn animation_id(&self, key: impl AsRef<str>) -> Option<AnimationId> {
        self.data
            .ids
            .get(key.as_ref())
            .and_then(|id| id.as_animation())
    }

    /// Gets the pak-unique `BitmapFontId` corresponding to the given key, if one exists.
    fn bitmap_font_id(&self, key: impl AsRef<str>) -> Option<BitmapFontId> {
        self.data
            .ids
            .get(key.as_ref())
            .and_then(|id| id.as_bitmap_font())
    }

    /// Gets the pak-unique `BitmapId` corresponding to the given key, if one exists.
    fn bitmap_id(&self, key: impl AsRef<str>) -> Option<BitmapId> {
        self.data
            .ids
            .get(key.as_ref())
            .and_then(|id| id.as_bitmap())
    }

    /// Gets the pak-unique `BlobId` corresponding to the given key, if one exists.
    fn blob_id(&self, key: impl AsRef<str>) -> Option<BlobId> {
        self.data.ids.get(key.as_ref()).and_then(|id| id.as_blob())
    }

    /// Gets the pak-unique `MaterialId` corresponding to the given key, if one exists.
    fn material_id(&self, key: impl AsRef<str>) -> Option<MaterialId> {
        self.data
            .ids
            .get(key.as_ref())
            .and_then(|id| id.as_material())
    }

    /// Gets the pak-unique `MeshId` corresponding to the given key, if one exists.
    fn mesh_id(&self, key: impl AsRef<str>) -> Option<MeshId> {
        self.data.ids.get(key.as_ref()).and_then(|id| id.as_mesh())
    }

    /// Gets the pak-unique `SceneId` corresponding to the given key, if one exists.
    fn scene_id(&self, key: impl AsRef<str>) -> Option<SceneId> {
        self.data.ids.get(key.as_ref()).and_then(|id| id.as_scene())
    }

    /// Gets the corresponding animation for the given ID.
    fn read_animation_id(&mut self, id: impl Into<AnimationId>) -> Result<Animation, Error> {
        let id = id.into();

        trace!("Deserializing animation {}", id.0);

        let range = self
            .data
            .anims
            .get(id.0)
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?
            .data_range()?;
        self.deserialize(&range)
    }

    /// Reads the corresponding bitmap for the given ID.
    fn read_bitmap_font_id(&mut self, id: impl Into<BitmapFontId>) -> Result<BitmapFont, Error> {
        let id = id.into();

        trace!("Deserializing bitmap font {}", id.0);

        let range = self
            .data
            .bitmap_fonts
            .get(id.0)
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?
            .data_range()?;
        self.deserialize(&range)
    }

    /// Reads the corresponding bitmap for the given ID.
    fn read_bitmap_id(&mut self, id: impl Into<BitmapId>) -> Result<Bitmap, Error> {
        let id = id.into();

        trace!("Deserializing bitmap {}", id.0);

        let (info, range) = {
            let bitmap = self
                .data
                .bitmaps
                .get(id.0)
                .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
            (bitmap.info, bitmap.raw.data_range()?)
        };
        let bitmap = self.deserialize(&range)?;
        if info.matches_raw(&bitmap) {
            Ok(bitmap)
        } else {
            Err(Error::from(ErrorKind::InvalidData))
        }
    }

    /// Gets the corresponding blob for the given ID.
    fn read_blob_id(&mut self, id: impl Into<BlobId>) -> Result<Vec<u8>, Error> {
        let id = id.into();

        trace!("Deserializing blob {}", id.0);

        let range = self
            .data
            .blobs
            .get(id.0)
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?
            .data_range()?;
        self.deserialize(&range)
    }

    fn stream_blob_id(&self, id: impl Into<BlobId>) -> Result<BlobStream, Error> {
        let id = id.into();
        let range = self
            .data
            .blobs
            .get(id.0)
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?
            .data_range()?;
        if range.compression.is_some() {
            return Err(Error::new(
                ErrorKind::Unsupported,
                "streaming compressed pak blobs is not supported",
            ));
        }
        let segment = self
            .readers
            .get(range.segment as usize)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "unknown pak segment ID"))?;
        let mut reader = segment.stream.open()?;
        let pos = segment
            .range_base
            .checked_add(range.range.start)
            .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
        let end = segment
            .range_base
            .checked_add(range.range.end)
            .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
        let stored_len = range
            .range
            .end
            .checked_sub(range.range.start)
            .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
        if pos < segment.payload_start || end > segment.payload_end {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "pak range lies outside segment payload bounds",
            ));
        }
        if stored_len < size_of::<u64>() as u64 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "blob range is shorter than its length prefix",
            ));
        }
        reader.seek(SeekFrom::Start(pos))?;
        let payload_len = read_bincode_legacy_len(&mut reader)?;
        if payload_len != stored_len - size_of::<u64>() as u64 {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "blob length prefix does not exactly match its stored range",
            ));
        }
        let start = reader.stream_position()?;

        Ok(BlobStream {
            reader,
            start,
            len: payload_len,
            pos: 0,
        })
    }

    /// Gets the material for the given ID.
    fn read_material_id(&self, id: impl Into<MaterialId>) -> Option<MaterialInfo> {
        let id = id.into();

        self.data.materials.get(id.0).cloned()
    }

    /// Gets the corresponding mesh for the given ID.
    fn read_mesh_id(&mut self, id: impl Into<MeshId>) -> Result<Mesh, Error> {
        let id = id.into();

        trace!("Deserializing mesh {}", id.0);

        let range = self
            .data
            .meshes
            .get(id.0)
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?
            .data_range()?;
        self.deserialize(&range)
    }

    /// Gets the corresponding animation for the given ID.
    fn read_scene_id(&mut self, id: impl Into<SceneId>) -> Result<Scene, Error> {
        let id = id.into();

        trace!("Deserializing scene {}", id.0);

        let range = self
            .data
            .scenes
            .get(id.0)
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?
            .data_range()?;
        self.deserialize(&range)
    }
}

#[derive(Debug)]
pub struct BlobStream {
    reader: Box<dyn Stream>,
    start: u64,
    len: u64,
    pos: u64,
}

impl BlobStream {
    pub fn len(&self) -> u64 {
        self.len
    }

    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

impl Read for BlobStream {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.len {
            return Ok(0);
        }

        let remaining = usize::try_from(self.len - self.pos).unwrap_or(usize::MAX);
        let count = buf.len().min(remaining);
        let count = self.reader.read(&mut buf[..count])?;
        self.pos += count as u64;
        Ok(count)
    }
}

impl Seek for BlobStream {
    fn seek(&mut self, pos: SeekFrom) -> std::io::Result<u64> {
        let next = match pos {
            SeekFrom::Start(pos) => pos,
            SeekFrom::End(offset) => self.len.saturating_add_signed(offset),
            SeekFrom::Current(offset) => self.pos.saturating_add_signed(offset),
        }
        .min(self.len);
        let absolute = self
            .start
            .checked_add(next)
            .ok_or_else(|| Error::from(ErrorKind::InvalidInput))?;
        self.reader.seek(SeekFrom::Start(absolute))?;
        self.pos = next;
        Ok(self.pos)
    }
}

fn read_bincode_legacy_len(reader: &mut dyn Read) -> Result<u64, Error> {
    let mut bytes = [0u8; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

#[derive(Debug)]
struct PakFile {
    buf: BufReader<File>,
}

impl From<&'static [u8]> for PakBuf {
    fn from(data: &'static [u8]) -> Self {
        Self::from_stream(Cursor::new(data)).expect("invalid pak data")
    }
}

pub trait Stream: Debug + Read + Seek + Send {
    fn open(&self) -> Result<Box<dyn Stream>, Error>;
}

impl Stream for PakFile {
    fn open(&self) -> Result<Box<dyn Stream>, Error> {
        let file = self.buf.get_ref().try_clone()?;
        Ok(Box::new(PakFile {
            buf: BufReader::new(file),
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

#[cfg(test)]
mod test {
    use super::*;

    fn opacity_micromap_info(recipe: u8) -> OpacityMicromapInfo {
        OpacityMicromapInfo {
            key: OpacityMicromapKey {
                mesh: MeshId(0),
                primitive: 0,
                material: MaterialId(0),
                source_mip: 0,
                recipe: opacity_micromap::OpacityMicromapRecipe([recipe; 32]),
            },
            payload: OpacityMicromapId(0),
        }
    }

    fn data_ref<T>(range: Range<u64>, compression: Option<Compression>) -> DataRef<T> {
        DataRef::Ref(DataRange {
            segment: 0,
            range,
            compression,
        })
    }

    fn empty_pak() -> PakBuf {
        PakBuf {
            data: Data::default(),
            readers: vec![SegmentReader {
                stream: Box::new(Cursor::new(&[] as &'static [u8])),
                range_base: 0,
                payload_start: 0,
                payload_end: u64::MAX,
                expected_hash: None,
            }],
        }
    }

    #[test]
    fn invalid_read_ids_return_invalid_input() {
        assert_eq!(
            empty_pak()
                .read_animation_id(AnimationId(0))
                .expect_err("invalid animation id should error")
                .kind(),
            ErrorKind::InvalidInput,
        );
        assert_eq!(
            empty_pak()
                .read_bitmap_font_id(BitmapFontId(0))
                .expect_err("invalid bitmap font id should error")
                .kind(),
            ErrorKind::InvalidInput,
        );
        assert_eq!(
            empty_pak()
                .read_bitmap_id(BitmapId(0))
                .expect_err("invalid bitmap id should error")
                .kind(),
            ErrorKind::InvalidInput,
        );
        assert_eq!(
            empty_pak()
                .read_compressed_bitmap_id(BitmapId(0))
                .expect_err("invalid compressed bitmap id should error")
                .kind(),
            ErrorKind::InvalidInput,
        );
        assert_eq!(
            empty_pak()
                .read_blob_id(BlobId(0))
                .expect_err("invalid blob id should error")
                .kind(),
            ErrorKind::InvalidInput,
        );
        assert_eq!(
            empty_pak()
                .read_mesh_id(MeshId(0))
                .expect_err("invalid mesh id should error")
                .kind(),
            ErrorKind::InvalidInput,
        );
        assert_eq!(
            empty_pak()
                .read_opacity_micromap_id(OpacityMicromapId(0))
                .expect_err("invalid opacity micromap id should error")
                .kind(),
            ErrorKind::InvalidInput,
        );
        assert_eq!(
            empty_pak()
                .read_scene_id(SceneId(0))
                .expect_err("invalid scene id should error")
                .kind(),
            ErrorKind::InvalidInput,
        );
    }

    #[test]
    fn opacity_micromap_header_keys_must_be_sorted_and_valid() {
        let mut data = Data::default();
        data.opacity_micromap_infos = vec![opacity_micromap_info(1), opacity_micromap_info(0)];
        assert_eq!(
            PakBuf::validate_header(&data).unwrap_err().kind(),
            ErrorKind::InvalidData
        );

        data.opacity_micromap_infos = vec![opacity_micromap_info(0)];
        assert_eq!(
            PakBuf::validate_header(&data).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn invalid_data_ref_range_returns_invalid_data() {
        let mut pak = empty_pak();
        let (start, end) = (10, 5);
        pak.data.blobs.push(data_ref(start..end, None));

        assert_eq!(
            pak.read_blob_id(BlobId(0))
                .expect_err("invalid blob range should error")
                .kind(),
            ErrorKind::InvalidData,
        );
    }

    #[test]
    fn trailing_asset_bytes_return_invalid_data() {
        let mut encoded = Vec::new();
        bincode::serde::encode_into_std_write(
            b"blob".to_vec(),
            &mut encoded,
            bincode::config::legacy(),
        )
        .unwrap();
        encoded.extend_from_slice(b"junk");
        let payload_len = encoded.len();
        encoded.extend_from_slice(&[0; PAK_HASH_LEN]);
        let encoded: &'static [u8] = Box::leak(encoded.into_boxed_slice());

        let mut pak = empty_pak();
        pak.data.blobs.push(data_ref(0..payload_len as u64, None));
        pak.readers[0].stream = Box::new(Cursor::new(encoded));

        assert_eq!(
            pak.read_blob_id(BlobId(0))
                .expect_err("trailing asset bytes should error")
                .kind(),
            ErrorKind::InvalidData,
        );
    }

    #[test]
    fn compressed_bitmap_read_skips_invalid_raw_range() {
        use crate::bitmap::{
            BitmapColor, BitmapCompression, BitmapFormat, CompressedBitmap, CompressedMip,
        };

        let compressed = CompressedBitmap::new(
            BitmapCompression::Bc1Srgb,
            vec![CompressedMip::new(1, 1, vec![7; 8])],
        );
        let source = Bitmap::new(BitmapColor::Srgb, BitmapFormat::Rgb, 1, 1, [1, 2, 3])
            .with_compressed(compressed.clone());
        let info = BitmapInfo::new(&source);
        let raw = vec![0xff; 16];
        let mut data = raw.clone();
        bincode::serde::encode_into_std_write(&compressed, &mut data, bincode::config::legacy())
            .unwrap();
        let payload_len = data.len();
        data.extend_from_slice(&[0; PAK_HASH_LEN]);
        let data: &'static [u8] = Box::leak(data.into_boxed_slice());

        let mut pak = empty_pak();
        pak.data.bitmaps.push(BitmapData {
            info,
            raw: data_ref(0..raw.len() as u64, None),
            compressed: Some(data_ref(raw.len() as u64..payload_len as u64, None)),
        });
        pak.readers[0].stream = Box::new(Cursor::new(data));

        assert_eq!(pak.bitmap_info_id(BitmapId(0)), Some(info));
        assert_eq!(
            pak.read_compressed_bitmap_id(BitmapId(0)).unwrap(),
            Some(compressed)
        );
        assert_eq!(
            pak.read_bitmap_id(BitmapId(0))
                .expect_err("raw sentinel should not deserialize")
                .kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn stream_blob_reads_payload_without_bincode_prefix() {
        let payload = vec![1, 2, 3, 4];
        let mut encoded = Vec::new();
        bincode::serde::encode_into_std_write(&payload, &mut encoded, bincode::config::legacy())
            .unwrap();
        let payload_len = encoded.len();
        encoded.extend_from_slice(&[0; PAK_HASH_LEN]);
        let stream_data: &'static [u8] = Box::leak(encoded.into_boxed_slice());
        let mut pak = empty_pak();
        pak.data.blobs.push(data_ref(0..payload_len as u64, None));
        pak.readers[0].stream = Box::new(Cursor::new(stream_data));

        assert_eq!(pak.read_blob_id(BlobId(0)).unwrap(), payload);

        let mut stream = pak.stream_blob_id(BlobId(0)).unwrap();
        assert_eq!(stream.len(), 4);
        stream.seek(SeekFrom::Start(1)).unwrap();
        let mut bytes = Vec::new();
        stream.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, [2, 3, 4]);
    }

    #[test]
    fn root_index_offset_cannot_precede_payload_prefix() {
        let mut encoded = Vec::new();
        bincode::serde::encode_into_std_write(
            *b"ATTACKGOAT-PAK-V1.8 ",
            &mut encoded,
            bincode::config::legacy(),
        )
        .unwrap();
        bincode::serde::encode_into_std_write(0_u64, &mut encoded, bincode::config::legacy())
            .unwrap();
        bincode::serde::encode_into_std_write(
            Option::<Compression>::None,
            &mut encoded,
            bincode::config::legacy(),
        )
        .unwrap();
        encoded.extend_from_slice(&[0; PAK_HASH_LEN]);
        let encoded: &'static [u8] = Box::leak(encoded.into_boxed_slice());

        assert_eq!(
            PakBuf::read_root(Cursor::new(encoded)).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn ranges_must_stay_inside_payload_bounds() {
        let mut pak = empty_pak();
        pak.readers[0].payload_start = 10;
        pak.readers[0].payload_end = 20;
        let range = DataRange {
            segment: 0,
            range: 0..1,
            compression: None,
        };

        assert_eq!(
            pak.deserialize::<Vec<u8>>(&range).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn stored_payload_allocation_is_bounded() {
        let mut pak = empty_pak();
        let range = DataRange {
            segment: 0,
            range: 0..MAX_STORED_PAYLOAD_BYTES + 1,
            compression: None,
        };

        assert_eq!(
            pak.deserialize::<Vec<u8>>(&range).unwrap_err().kind(),
            ErrorKind::InvalidData
        );
    }

    #[test]
    fn blob_stream_requires_exact_length_prefix_and_range() {
        for payload in [vec![0; 7], {
            let mut payload = 1_u64.to_le_bytes().to_vec();
            payload.extend_from_slice(&[1, 2]);
            payload
        }] {
            let stored_len = payload.len() as u64;
            let mut bytes = payload;
            bytes.extend_from_slice(&[0; PAK_HASH_LEN]);
            let bytes: &'static [u8] = Box::leak(bytes.into_boxed_slice());
            let mut pak = empty_pak();
            pak.data.blobs.push(data_ref(0..stored_len, None));
            pak.readers[0] = SegmentReader {
                stream: Box::new(Cursor::new(bytes)),
                range_base: 0,
                payload_start: 0,
                payload_end: stored_len,
                expected_hash: None,
            };

            assert_eq!(
                pak.stream_blob_id(BlobId(0)).unwrap_err().kind(),
                ErrorKind::InvalidData
            );
        }
    }
}
