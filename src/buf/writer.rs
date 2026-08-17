use {
    super::{super::compression::Compression, Asset},
    crate::{
        AnimationId, BitmapData, BitmapFontId, BitmapId, BlobId, Data, DataRange, DataRef, Id,
        MaterialId, MaterialInfo, MeshId, SceneId, SegmentMeta,
        anim::Animation,
        bitmap::{Bitmap, BitmapInfo},
        bitmap_font::BitmapFont,
        mesh::Mesh,
        pak_hash_stream,
        scene::Scene,
    },
    anyhow::bail,
    parking_lot::Mutex,
    serde::Serialize,
    std::{
        collections::HashMap,
        fs::{File, OpenOptions, create_dir, remove_dir_all},
        io::{Error, ErrorKind, Read, Seek, SeekFrom, Write, copy},
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicU64, Ordering},
        },
    },
    tempfile::NamedTempFile,
};

#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

static TEMP_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub(super) struct StoragePolicy {
    pub(super) segment: u16,
    pub(super) compression: Option<Compression>,
    pub(super) texture_compression: bool,
}

#[derive(Default)]
pub struct Writer {
    header_compression: Option<Compression>,
    active_policy: StoragePolicy,
    direct_policies: HashMap<Asset, StoragePolicy>,
    ctx: HashMap<Asset, AssetEntry>,
    segment_names: Vec<String>,
    spool: Option<Spool>,
    data: Data,
}

#[derive(Clone, Copy)]
struct AssetEntry {
    id: Id,
    policy: StoragePolicy,
}

struct Spool {
    dir: PathBuf,
    files: HashMap<u16, File>,
    entries: HashMap<u16, Vec<(DataRange, Option<Compression>)>>,
}

impl Spool {
    fn new() -> Result<Self, Error> {
        let base = std::env::temp_dir();
        for _ in 0..128 {
            let dir = base.join(format!(
                "pak-spool-{}-{}",
                std::process::id(),
                TEMP_ID.fetch_add(1, Ordering::Relaxed)
            ));
            match create_dir(&dir) {
                Ok(()) => {
                    #[cfg(unix)]
                    std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o700))?;
                    return Ok(Self {
                        dir,
                        files: HashMap::new(),
                        entries: HashMap::new(),
                    });
                }
                Err(error) if error.kind() == ErrorKind::AlreadyExists => continue,
                Err(error) => return Err(error),
            }
        }
        Err(Error::new(
            ErrorKind::AlreadyExists,
            "unable to create pak spool directory",
        ))
    }

    fn file(&mut self, segment: u16) -> Result<&mut File, Error> {
        if !self.files.contains_key(&segment) {
            let mut options = OpenOptions::new();
            options.create_new(true).read(true).write(true);
            #[cfg(unix)]
            options.mode(0o600);
            let file = options.open(self.dir.join(format!("{segment}.payload")))?;
            self.files.insert(segment, file);
        }
        Ok(self
            .files
            .get_mut(&segment)
            .expect("spool file was inserted"))
    }

    fn copy_to(&mut self, segment: u16, output: &mut impl Write) -> Result<u64, Error> {
        let Some(file) = self.files.get_mut(&segment) else {
            return Ok(0);
        };
        file.flush()?;
        file.seek(SeekFrom::Start(0))?;
        copy(file, output)
    }

    fn segment_generation(&mut self, segment: u16, name: &str) -> Result<[u8; 32], Error> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"pak-segment-generation/v2");
        hasher.update(&segment.to_le_bytes());
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        if let Some(entries) = self.entries.get(&segment) {
            hasher.update(&(entries.len() as u64).to_le_bytes());
            for (range, compression) in entries {
                hasher.update(&range.range.start.to_le_bytes());
                hasher.update(&range.range.end.to_le_bytes());
                let encoded = bincode::serde::encode_to_vec(compression, bincode::config::legacy())
                    .map_err(|_| Error::from(ErrorKind::InvalidData))?;
                hasher.update(&(encoded.len() as u64).to_le_bytes());
                hasher.update(&encoded);
            }
        } else {
            hasher.update(&0u64.to_le_bytes());
        }
        if let Some(file) = self.files.get_mut(&segment) {
            file.flush()?;
            file.seek(SeekFrom::Start(0))?;
            let mut buffer = [0; 8192];
            loop {
                let read = file.read(&mut buffer)?;
                if read == 0 {
                    break;
                }
                hasher.update(&buffer[..read]);
            }
            file.seek(SeekFrom::End(0))?;
        }
        Ok(*hasher.finalize().as_bytes())
    }
}

impl Drop for Spool {
    fn drop(&mut self) {
        self.files.clear();
        let _ = remove_dir_all(&self.dir);
    }
}

impl Writer {
    pub fn push_animation(
        &mut self,
        animation: Animation,
        policy: StoragePolicy,
    ) -> Result<AnimationId, Error> {
        let id = AnimationId(self.data.anims.len());
        let data = self.spool(&animation, policy)?;
        self.data.anims.push(data);

        Ok(id)
    }

    pub fn push_bitmap_font(
        &mut self,
        bitmap_font: BitmapFont,
        policy: StoragePolicy,
    ) -> Result<BitmapFontId, Error> {
        let id = BitmapFontId(self.data.bitmap_fonts.len());
        let data = self.spool(&bitmap_font, policy)?;
        self.data.bitmap_fonts.push(data);

        Ok(id)
    }

    pub fn push_bitmap(
        &mut self,
        bitmap: Bitmap,
        policy: StoragePolicy,
    ) -> Result<BitmapId, Error> {
        let id = BitmapId(self.data.bitmaps.len());
        let info = BitmapInfo::new(&bitmap);
        let (raw, compressed) = bitmap.into_variants();
        let raw = self.spool(&raw, policy)?;
        let compressed = compressed
            .as_ref()
            .map(|data| {
                self.spool(
                    data,
                    StoragePolicy {
                        compression: None,
                        ..policy
                    },
                )
            })
            .transpose()?;
        self.data.bitmaps.push(BitmapData {
            info,
            raw,
            compressed,
        });

        Ok(id)
    }

    pub fn push_blob(&mut self, blob: Vec<u8>, policy: StoragePolicy) -> Result<BlobId, Error> {
        let id = BlobId(self.data.blobs.len());
        let data = self.spool(&blob, policy)?;
        self.data.blobs.push(data);

        Ok(id)
    }

    pub fn push_material(&mut self, info: MaterialInfo) -> MaterialId {
        let id = MaterialId(self.data.materials.len());
        self.data.materials.push(info);

        id
    }

    pub fn push_mesh(&mut self, mesh: Mesh, policy: StoragePolicy) -> Result<MeshId, Error> {
        let id = MeshId(self.data.meshes.len());
        let data = self.spool(&mesh, policy)?;
        self.data.meshes.push(data);

        Ok(id)
    }

    pub fn push_scene(&mut self, scene: Scene, policy: StoragePolicy) -> Result<SceneId, Error> {
        let id = SceneId(self.data.scenes.len());
        let data = self.spool(&scene, policy)?;
        self.data.scenes.push(data);

        Ok(id)
    }

    #[allow(dead_code)]
    pub fn with_compression(&mut self, compression: Compression) -> &mut Self {
        self.header_compression = Some(compression);
        self
    }

    pub fn with_compression_is(&mut self, compression: Option<Compression>) -> &mut Self {
        self.header_compression = compression;
        self
    }

    pub(super) fn set_segments(&mut self, names: Vec<String>) -> anyhow::Result<()> {
        if names.len() > u16::MAX as usize {
            bail!("too many pak segments");
        }
        self.segment_names = names;
        Ok(())
    }

    pub(super) fn register_direct_policy(
        &mut self,
        asset: Asset,
        policy: StoragePolicy,
    ) -> anyhow::Result<()> {
        if let Some(existing) = self.direct_policies.insert(asset.clone(), policy)
            && existing != policy
        {
            bail!(
                "asset {asset:?} is directly declared with conflicting storage policies: {existing:?} and {policy:?}"
            );
        }
        Ok(())
    }

    pub(super) fn set_active_policy(&mut self, policy: StoragePolicy) {
        self.active_policy = policy;
    }

    pub(super) fn policy_for(&self, asset: &Asset) -> StoragePolicy {
        self.direct_policies
            .get(asset)
            .copied()
            .unwrap_or(self.active_policy)
    }

    pub(super) fn with_asset_policy<T>(
        writer: &Arc<Mutex<Self>>,
        asset: &Asset,
        bake_dependencies: impl FnOnce() -> anyhow::Result<T>,
    ) -> anyhow::Result<T> {
        let previous = {
            let mut writer = writer.lock();
            let policy = writer.policy_for(asset);
            std::mem::replace(&mut writer.active_policy, policy)
        };
        let result = bake_dependencies();
        writer.lock().active_policy = previous;
        result
    }

    pub(super) fn asset_id(
        &mut self,
        asset: &Asset,
        key: Option<&str>,
    ) -> anyhow::Result<Option<Id>> {
        let requested = self.policy_for(asset);
        let Some(entry) = self.ctx.get(asset).copied() else {
            return Ok(None);
        };
        if entry.policy != requested {
            bail!(
                "asset {asset:?} was requested with conflicting inherited storage policies: {:?} and {:?}",
                entry.policy,
                requested,
            );
        }
        if let Some(key) = key {
            self.add_alias(key.to_owned(), entry.id)?;
        }
        Ok(Some(entry.id))
    }

    pub(super) fn commit_asset(
        &mut self,
        asset: Asset,
        id: impl Into<Id>,
        key: Option<String>,
    ) -> anyhow::Result<()> {
        let id = id.into();
        let policy = self.policy_for(&asset);
        if let Some(existing) = self.ctx.insert(asset.clone(), AssetEntry { id, policy })
            && (existing.id != id || existing.policy != policy)
        {
            bail!("asset {asset:?} committed more than once with different IDs or policies");
        }
        if let Some(key) = key {
            self.add_alias(key, id)?;
        }
        Ok(())
    }

    fn add_alias(&mut self, key: String, id: Id) -> anyhow::Result<()> {
        if let Some(existing) = self.data.ids.get(&key) {
            if *existing != id {
                bail!("pak key {key:?} maps to two different asset IDs");
            }
        } else {
            self.data.ids.insert(key, id);
        }
        Ok(())
    }

    pub(super) fn texture_compression(&self) -> bool {
        self.active_policy.texture_compression
    }

    pub fn write(&mut self, path: impl AsRef<Path>) -> Result<(), Error> {
        let path = path.as_ref();
        let parent = path.parent().unwrap_or_else(|| Path::new("."));
        let mut data = self.data.clone();
        let main_generation = self.segment_generation(0, "")?;
        data.segments = Vec::with_capacity(self.segment_names.len());
        for index in 0..self.segment_names.len() {
            let name = self.segment_names[index].clone();
            let generation = self.segment_generation(index as u16 + 1, &name)?;
            let generation_hex = generation
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>();
            data.segments.push(SegmentMeta {
                name: name.clone(),
                filename: format!("{name}.{generation_hex}.pakseg"),
                generation,
                hash: 0,
            });
        }

        for index in 0..data.segments.len() {
            let name = data.segments[index].name.clone();
            let generation = data.segments[index].generation;
            let target = parent.join(&data.segments[index].filename);
            let mut temporary = NamedTempFile::new_in(parent)?;
            self.write_sidecar(temporary.as_file_mut(), index as u16 + 1, &name, generation)?;
            let expected_hash = Self::finish_hash(temporary.as_file_mut())?;
            let expected_len = temporary.as_file().metadata()?.len();
            data.segments[index].hash = expected_hash;
            if Self::sidecar_is_reusable(
                &target,
                index as u16 + 1,
                &name,
                generation,
                expected_hash,
                expected_len,
            )? {
                continue;
            }
            match temporary.persist_noclobber(&target) {
                Ok(_) => (),
                Err(error) if error.error.kind() == ErrorKind::AlreadyExists => {
                    if !Self::sidecar_is_reusable(
                        &target,
                        index as u16 + 1,
                        &name,
                        generation,
                        expected_hash,
                        expected_len,
                    )? {
                        return Err(Error::new(
                            ErrorKind::AlreadyExists,
                            "generation sidecar appeared with unexpected contents",
                        ));
                    }
                }
                Err(error) => return Err(error.error),
            }
        }
        data.generation = self.archive_generation(&data, main_generation)?;

        let mut root = NamedTempFile::new_in(parent)?;
        self.write_root(root.as_file_mut(), &mut data)?;
        Self::finish_hash(root.as_file_mut())?;
        root.persist(path).map_err(|error| error.error)?;
        if let Ok(directory) = File::open(parent) {
            let _ = directory.sync_all();
        }
        Ok(())
    }

    fn segment_generation(&mut self, segment: u16, name: &str) -> Result<[u8; 32], Error> {
        if let Some(spool) = &mut self.spool {
            return spool.segment_generation(segment, name);
        }
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"pak-segment-generation/v2");
        hasher.update(&segment.to_le_bytes());
        hasher.update(&(name.len() as u64).to_le_bytes());
        hasher.update(name.as_bytes());
        hasher.update(&0u64.to_le_bytes());
        Ok(*hasher.finalize().as_bytes())
    }

    fn archive_generation(
        &self,
        data: &Data,
        main_generation: [u8; 32],
    ) -> Result<[u8; 32], Error> {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"pak-archive-generation/v2");
        let metadata = bincode::serde::encode_to_vec(
            (&self.header_compression, data),
            bincode::config::legacy(),
        )
        .map_err(|_| Error::from(ErrorKind::InvalidData))?;
        hasher.update(&(metadata.len() as u64).to_le_bytes());
        hasher.update(&metadata);
        hasher.update(&main_generation);
        Ok(*hasher.finalize().as_bytes())
    }

    fn spool<T>(&mut self, data: &T, policy: StoragePolicy) -> Result<DataRef<T>, Error>
    where
        T: Serialize,
    {
        if policy.segment as usize > self.segment_names.len() {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "unknown pak segment ID",
            ));
        }
        if self.spool.is_none() {
            self.spool = Some(Spool::new()?);
        }
        let file = self
            .spool
            .as_mut()
            .expect("spool was initialized")
            .file(policy.segment)?;
        let start = file.seek(SeekFrom::End(0))?;
        {
            let mut output = if let Some(compression) = policy.compression {
                compression.new_writer(&mut *file)
            } else {
                Box::new(&mut *file)
            };
            bincode::serde::encode_into_std_write(data, &mut output, bincode::config::legacy())
                .map_err(|_| Error::from(ErrorKind::InvalidData))?;
            output.flush()?;
        }
        let end = file.stream_position()?;
        let range = DataRange {
            segment: policy.segment,
            range: start..end,
            compression: policy.compression,
        };
        self.spool
            .as_mut()
            .expect("spool was initialized")
            .entries
            .entry(policy.segment)
            .or_default()
            .push((range.clone(), policy.compression));
        Ok(DataRef::Ref(range))
    }

    fn sidecar_is_reusable(
        target: &Path,
        id: u16,
        name: &str,
        generation: [u8; 32],
        expected_hash: u64,
        expected_len: u64,
    ) -> Result<bool, Error> {
        let mut file = match File::open(target) {
            Ok(file) => file,
            Err(error) if error.kind() == ErrorKind::NotFound => return Ok(false),
            Err(error) => return Err(error),
        };
        let magic = bincode::serde::decode_from_std_read::<[u8; 20], _, _>(
            &mut file,
            bincode::config::legacy(),
        );
        let existing_id =
            bincode::serde::decode_from_std_read::<u16, _, _>(&mut file, bincode::config::legacy());
        let existing_name = bincode::serde::decode_from_std_read::<String, _, _>(
            &mut file,
            bincode::config::legacy(),
        );
        let existing_generation = bincode::serde::decode_from_std_read::<[u8; 32], _, _>(
            &mut file,
            bincode::config::legacy(),
        );
        if magic.as_ref().ok() != Some(b"ATTACKGOAT-SEG-V1.2 ")
            || existing_id.ok() != Some(id)
            || existing_name.as_deref().ok() != Some(name)
            || existing_generation.ok() != Some(generation)
        {
            return Err(Error::new(
                ErrorKind::AlreadyExists,
                format!("generation sidecar identity mismatch: {}", target.display()),
            ));
        }
        let length = file.seek(SeekFrom::End(0))?;
        if length != expected_len {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("generation sidecar length mismatch: {}", target.display()),
            ));
        }
        let payload_len = length
            .checked_sub(crate::PAK_HASH_LEN as u64)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "truncated generation sidecar"))?;
        file.seek(SeekFrom::Start(0))?;
        let actual = pak_hash_stream(&mut file, payload_len)?;
        let expected = crate::read_hash_trailer(&mut file)?;
        if actual != expected || actual != expected_hash {
            return Err(Error::new(
                ErrorKind::InvalidData,
                format!("generation sidecar hash mismatch: {}", target.display()),
            ));
        }
        Ok(true)
    }

    fn write_root(
        &mut self,
        mut writer: &mut (impl Write + Seek),
        data: &mut Data,
    ) -> Result<(), Error> {
        let mut magic_bytes = [0u8; 20];
        magic_bytes.copy_from_slice(b"ATTACKGOAT-PAK-V1.6 ");

        // Write a known value so we can identify this file
        bincode::serde::encode_into_std_write(magic_bytes, &mut writer, bincode::config::legacy())
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;

        let skip_position = writer.stream_position()?;

        // Write a blank spot that we'll use for the skip header later
        bincode::serde::encode_into_std_write(0u64, &mut writer, bincode::config::legacy())
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;

        // Write the compression we're going to be using, if any
        bincode::serde::encode_into_std_write(
            self.header_compression,
            &mut writer,
            bincode::config::legacy(),
        )
        .map_err(|_| Error::from(ErrorKind::InvalidData))?;

        let payload_start = writer.stream_position()?;
        Self::offset_root_ranges(data, payload_start)?;
        if let Some(spool) = &mut self.spool {
            spool.copy_to(0, writer)?;
        }
        let skip = writer.stream_position()?;
        {
            let mut compressed = if let Some(compressed) = self.header_compression {
                compressed.new_writer(&mut writer)
            } else {
                Box::new(&mut writer)
            };
            bincode::serde::encode_into_std_write(data, &mut compressed, bincode::config::legacy())
                .map_err(|_| Error::from(ErrorKind::InvalidData))?;
            compressed.flush()?;
        }

        writer.seek(SeekFrom::Start(skip_position))?;
        bincode::serde::encode_into_std_write(skip, &mut writer, bincode::config::legacy())
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;

        Ok(())
    }

    fn write_sidecar(
        &mut self,
        writer: &mut (impl Write + Seek),
        segment: u16,
        name: &str,
        generation: [u8; 32],
    ) -> Result<(), Error> {
        bincode::serde::encode_into_std_write(
            *b"ATTACKGOAT-SEG-V1.2 ",
            &mut *writer,
            bincode::config::legacy(),
        )
        .map_err(|_| Error::from(ErrorKind::InvalidData))?;
        bincode::serde::encode_into_std_write(segment, &mut *writer, bincode::config::legacy())
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;
        bincode::serde::encode_into_std_write(name, &mut *writer, bincode::config::legacy())
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;
        bincode::serde::encode_into_std_write(generation, &mut *writer, bincode::config::legacy())
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;
        if let Some(spool) = &mut self.spool {
            spool.copy_to(segment, writer)?;
        }
        Ok(())
    }

    fn offset_root_ranges(data: &mut Data, base: u64) -> Result<(), Error> {
        fn offset<T>(data: &mut DataRef<T>, base: u64) -> Result<(), Error> {
            if let DataRef::Ref(range) = data
                && range.segment == 0
            {
                range.range.start = range.range.start.checked_add(base).ok_or_else(|| {
                    Error::new(ErrorKind::InvalidData, "pak payload offset overflow")
                })?;
                range.range.end = range.range.end.checked_add(base).ok_or_else(|| {
                    Error::new(ErrorKind::InvalidData, "pak payload offset overflow")
                })?;
            }
            Ok(())
        }
        for range in &mut data.anims {
            offset(range, base)?;
        }
        for bitmap in &mut data.bitmaps {
            offset(&mut bitmap.raw, base)?;
            if let Some(data) = &mut bitmap.compressed {
                offset(data, base)?;
            }
        }
        for range in &mut data.blobs {
            offset(range, base)?;
        }
        for range in &mut data.bitmap_fonts {
            offset(range, base)?;
        }
        for range in &mut data.meshes {
            offset(range, base)?;
        }
        for range in &mut data.scenes {
            offset(range, base)?;
        }
        Ok(())
    }

    fn finish_hash(file: &mut File) -> Result<u64, Error> {
        file.flush()?;
        let len = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;
        let hash = pak_hash_stream(file, len)?;
        file.seek(SeekFrom::End(0))?;
        bincode::serde::encode_into_std_write(hash, file, bincode::config::legacy())
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;
        file.sync_all()?;
        Ok(hash)
    }
}

#[cfg(test)]
mod test {
    use {
        super::{StoragePolicy, Writer},
        crate::buf::{Asset, blob::BlobAsset},
        crate::{
            BlobId,
            compression::{BrotliParams, Compression},
        },
        std::{fs, path::PathBuf},
    };

    fn asset() -> Asset {
        Asset::Blob(BlobAsset::new(PathBuf::from("dependency.bin")))
    }

    fn policy(compression: Option<Compression>) -> StoragePolicy {
        StoragePolicy {
            segment: 0,
            compression,
            texture_compression: false,
        }
    }

    #[test]
    fn conflicting_inherited_dependency_policies_error() {
        let mut writer = Writer::default();
        let asset = asset();
        writer.set_active_policy(policy(Some(Compression::Snap)));
        let id = writer
            .push_blob(vec![1], policy(Some(Compression::Snap)))
            .unwrap();
        writer.commit_asset(asset.clone(), id, None).unwrap();
        writer.set_active_policy(policy(Some(Compression::Brotli(BrotliParams::default()))));

        let error = writer.asset_id(&asset, None).unwrap_err().to_string();
        assert!(error.contains("conflicting inherited storage policies"));
    }

    #[test]
    fn late_explicit_asset_adds_alias_to_existing_id() {
        let mut writer = Writer::default();
        let asset = asset();
        let id = writer.push_blob(vec![1], policy(None)).unwrap();
        writer.commit_asset(asset.clone(), id, None).unwrap();

        assert_eq!(
            writer.asset_id(&asset, Some("explicit")).unwrap(),
            Some(id.into())
        );
        assert_eq!(
            writer.data.ids.get("explicit").copied(),
            Some(BlobId(0).into())
        );
    }

    #[test]
    fn globally_known_direct_policy_wins_over_inherited_policy() {
        let mut writer = Writer::default();
        let asset = asset();
        writer
            .register_direct_policy(asset.clone(), policy(Some(Compression::Snap)))
            .unwrap();
        writer.set_active_policy(policy(Some(Compression::Brotli(BrotliParams::default()))));

        assert_eq!(writer.policy_for(&asset), policy(Some(Compression::Snap)));
        let id = writer
            .push_blob(vec![1], policy(Some(Compression::Snap)))
            .unwrap();
        writer.commit_asset(asset.clone(), id, None).unwrap();
        assert_eq!(writer.asset_id(&asset, None).unwrap(), Some(id.into()));
    }

    #[test]
    fn blob_and_bitmap_font_have_distinct_typed_identity() {
        let mut writer = Writer::default();
        let source = BlobAsset::new(PathBuf::from("font.fnt"));
        let blob = Asset::Blob(source.clone());
        let font = Asset::BitmapFont(source);
        let id = writer.push_blob(vec![1], policy(None)).unwrap();
        writer.commit_asset(blob, id, None).unwrap();

        assert!(writer.asset_id(&font, None).unwrap().is_none());
    }

    #[test]
    fn pushed_payload_is_replaced_by_spooled_range() {
        let mut writer = Writer::default();
        writer.push_blob(vec![7; 1024], policy(None)).unwrap();

        assert!(matches!(writer.data.blobs[0], crate::DataRef::Ref(_)));
        assert!(writer.spool.is_some());
    }

    #[test]
    fn repeated_write_does_not_mutate_ranges_or_change_output() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("repeat.pak");
        let mut writer = Writer::default();
        writer.push_blob(vec![7; 1024], policy(None)).unwrap();
        let range = writer.data.blobs[0].data_range().unwrap();

        writer.write(&destination).unwrap();
        let first = fs::read(&destination).unwrap();
        assert_eq!(writer.data.blobs[0].data_range().unwrap(), range);
        writer.write(&destination).unwrap();
        assert_eq!(fs::read(&destination).unwrap(), first);
        assert_eq!(writer.data.blobs[0].data_range().unwrap(), range);
    }

    #[cfg(unix)]
    #[test]
    fn spool_uses_private_unix_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let mut writer = Writer::default();
        writer.push_blob(vec![1], policy(None)).unwrap();
        let spool = writer.spool.as_ref().unwrap();
        assert_eq!(
            fs::metadata(&spool.dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
        let file = spool.dir.join("0.payload");
        assert_eq!(
            fs::metadata(file).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }
}
