use {
    super::{super::compression::Compression, Asset},
    crate::{
        AnimationId, BitmapData, BitmapFontId, BitmapId, BlobId, Data, DataRange, DataRef, Id,
        MaterialId, MaterialInfo, MeshId, OpacityMicromapId, SceneId, SegmentMeta,
        anim::Animation,
        bitmap::{Bitmap, BitmapInfo},
        bitmap_font::BitmapFont,
        mesh::Mesh,
        opacity_micromap::{OpacityMicromap, OpacityMicromapInfo, OpacityMicromapKey},
        pak_hash_stream,
        scene::Scene,
    },
    anyhow::{Context as _, bail},
    parking_lot::Mutex,
    serde::{Serialize, de::DeserializeOwned},
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
    pub(super) lod: super::mesh::lod::LodSettings,
    pub(super) default_lods: Box<[super::mesh::lod::LodRequest]>,
    header_compression: Option<Compression>,
    active_policy: StoragePolicy,
    direct_policies: HashMap<Asset, StoragePolicy>,
    ctx: HashMap<Asset, AssetEntry>,
    segment_names: Vec<String>,
    mesh_policies: Vec<StoragePolicy>,
    mesh_spool: Option<Spool>,
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

    fn read<T>(&mut self, range: &DataRange) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        let len = range
            .range
            .end
            .checked_sub(range.range.start)
            .ok_or_else(|| Error::from(ErrorKind::InvalidData))?;
        if len > crate::MAX_STORED_PAYLOAD_BYTES {
            return Err(Error::new(
                ErrorKind::InvalidData,
                "stored pak payload exceeds allocation limit",
            ));
        }
        let file = self
            .files
            .get_mut(&range.segment)
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "unknown pak segment ID"))?;
        file.flush()?;
        file.seek(SeekFrom::Start(range.range.start))?;
        let mut stored = file.take(len);

        let decoded = if let Some(compression) = range.compression {
            let mut reader = compression.new_reader(&mut stored);
            let decoded =
                bincode::serde::decode_from_std_read(&mut reader, bincode::config::legacy())
                    .map_err(|_| Error::from(ErrorKind::InvalidData))?;
            ensure_reader_end(&mut reader)?;
            decoded
        } else {
            let decoded =
                bincode::serde::decode_from_std_read(&mut stored, bincode::config::legacy())
                    .map_err(|_| Error::from(ErrorKind::InvalidData))?;
            ensure_reader_end(&mut stored)?;
            decoded
        };

        Ok(decoded)
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
        // Keep unfinished meshes out of archive payloads until the metadata hook has run.
        std::mem::swap(&mut self.spool, &mut self.mesh_spool);
        let data = self.spool(&mesh, policy);
        std::mem::swap(&mut self.spool, &mut self.mesh_spool);
        let data = data?;
        self.data.meshes.push(data);
        self.mesh_policies.push(policy);

        Ok(id)
    }

    #[allow(dead_code)]
    pub fn push_opacity_micromap(
        &mut self,
        key: OpacityMicromapKey,
        payload: OpacityMicromap,
        policy: StoragePolicy,
    ) -> Result<OpacityMicromapId, Error> {
        payload
            .validate()
            .map_err(|message| Error::new(ErrorKind::InvalidInput, message))?;
        if self
            .data
            .opacity_micromap_infos
            .last()
            .is_some_and(|info| info.key >= key)
        {
            return Err(Error::new(
                ErrorKind::InvalidInput,
                "opacity micromap keys must be pushed in strictly sorted order",
            ));
        }
        let id = OpacityMicromapId(self.data.opacity_micromaps.len());
        let data = self.spool(&payload, policy)?;
        self.data.opacity_micromaps.push(data);
        self.data
            .opacity_micromap_infos
            .push(OpacityMicromapInfo { key, payload: id });
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
        self.finalize_meshes(None).map_err(Error::other)?;
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

    fn finalize_meshes(
        &mut self,
        mut baker: Option<&mut dyn super::DerivedAssetBaker>,
    ) -> anyhow::Result<()> {
        let Some(mut spool) = self.mesh_spool.take() else {
            return Ok(());
        };

        for idx in 0..self.data.meshes.len() {
            let source = self.data.meshes[idx].data_range()?;
            let mut mesh: Mesh = spool.read(&source)?;
            if let Some(baker) = baker.as_deref_mut() {
                baker
                    .bake_mesh(&mut mesh)
                    .with_context(|| format!("baking mesh metadata for mesh {idx}"))?;
            }

            self.data.meshes[idx] = self.spool(&mesh, self.mesh_policies[idx])?;
        }

        Ok(())
    }

    pub(super) fn bake_derived_assets(
        &mut self,
        baker: &mut dyn super::DerivedAssetBaker,
    ) -> anyhow::Result<()> {
        self.finalize_meshes(Some(baker))?;

        let mut candidates = std::collections::BTreeSet::new();
        for scene in self.data.scenes.clone() {
            let scene: Scene = self.read_spooled(&scene)?;
            for reference in scene.refs() {
                let Some(mesh_id) = reference.mesh() else {
                    continue;
                };
                let Some(mesh) = self.data.meshes.get(mesh_id.0).cloned() else {
                    continue;
                };
                let mesh: Mesh = self.read_spooled(&mesh)?;
                for (primitive_index, primitive) in mesh.primitives().iter().enumerate() {
                    let Some(material) = reference
                        .materials()
                        .get(primitive.material() as usize)
                        .copied()
                    else {
                        continue;
                    };
                    candidates.insert((
                        mesh_id,
                        u32::try_from(primitive_index).map_err(|_| {
                            anyhow::anyhow!("mesh contains too many primitives for derived assets")
                        })?,
                        material,
                    ));
                }
            }
        }

        let mut outputs = Vec::new();
        for (mesh_id, primitive_index, material_id) in candidates {
            let Some(material) = self.data.materials.get(material_id.0) else {
                continue;
            };
            if !material.alpha_test {
                continue;
            }
            let color = material.color;
            let Some(mesh) = self.data.meshes.get(mesh_id.0).cloned() else {
                continue;
            };
            let mesh: Mesh = self.read_spooled(&mesh)?;
            let Some(primitive) = mesh.primitives().get(primitive_index as usize) else {
                continue;
            };
            let base = primitive.base();
            let indices = base.indices();
            let Some(texture0) = base.texture0() else {
                continue;
            };
            let Some(bitmap) = self.data.bitmaps.get(color.0) else {
                continue;
            };
            let Some(compressed) = bitmap.compressed.clone() else {
                continue;
            };
            let compressed = self.read_spooled(&compressed)?;
            if compressed.format() != crate::bitmap::BitmapCompression::Bc3 {
                continue;
            }

            let candidate = super::DerivedAssetBakeCandidate {
                alpha_bitmap: &compressed,
                indices: indices.as_u32().into_boxed_slice(),
                material: material_id,
                mesh: mesh_id,
                primitive: primitive_index,
                texture0: texture0.collect(),
            };
            let policy =
                self.mesh_policies.get(mesh_id.0).copied().ok_or_else(|| {
                    anyhow::anyhow!("derived asset source mesh policy is missing")
                })?;
            for output in baker.bake(&candidate)? {
                if output.source_mip as usize >= compressed.mips().len() {
                    bail!("derived asset source mip is out of bounds");
                }
                let key = OpacityMicromapKey {
                    mesh: mesh_id,
                    primitive: primitive_index,
                    material: material_id,
                    source_mip: output.source_mip,
                    recipe: output.recipe,
                };
                outputs.push((key, output.payload, policy));
            }
        }

        outputs.sort_by_key(|(key, _, _)| *key);
        for (key, payload, policy) in outputs {
            self.push_opacity_micromap(key, payload, policy)?;
        }
        Ok(())
    }

    fn read_spooled<T>(&mut self, data: &DataRef<T>) -> Result<T, Error>
    where
        T: DeserializeOwned,
    {
        let range = data.data_range()?;
        self.spool
            .as_mut()
            .ok_or_else(|| Error::new(ErrorKind::InvalidData, "pak spool is missing"))?
            .read(&range)
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
        magic_bytes.copy_from_slice(b"ATTACKGOAT-PAK-V1.10");

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
        for range in &mut data.opacity_micromaps {
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

fn ensure_reader_end(reader: &mut impl Read) -> Result<(), Error> {
    let mut trailing = [0; 1];
    match reader.read(&mut trailing)? {
        0 => Ok(()),
        _ => Err(Error::new(
            ErrorKind::InvalidData,
            "trailing bytes after deserialized data",
        )),
    }
}

#[cfg(test)]
mod test {
    use {
        super::{StoragePolicy, Writer},
        crate::buf::{
            Asset, Canonicalize as _, DerivedAssetBakeCandidate, DerivedAssetBakeOutput,
            DerivedAssetBaker, blob::BlobAsset, mesh::MeshAsset,
        },
        crate::{
            BlobId, MaterialInfo, MaterialParameterFlags, Pak as _, PakBuf,
            bitmap::{
                Bitmap, BitmapColor, BitmapCompression, BitmapFormat, CompressedBitmap,
                CompressedMip,
            },
            compression::{BrotliParams, Compression},
            index::IndexBuffer,
            mesh::{Geometry, Joint, Mesh, Primitive, Skin, VertexType},
            opacity_micromap::{OpacityMicromap, OpacityMicromapKey, OpacityMicromapRecipe},
            scene::{ReferenceData, Scene},
        },
        parking_lot::Mutex,
        std::{fs, io::ErrorKind, path::PathBuf, sync::Arc},
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

    fn opacity_micromap() -> OpacityMicromap {
        OpacityMicromap::new([], [], [], [-1], [], 1, 0).unwrap()
    }

    fn opacity_micromap_key(recipe: u8) -> OpacityMicromapKey {
        OpacityMicromapKey {
            mesh: crate::MeshId(0),
            primitive: 0,
            material: crate::MaterialId(0),
            source_mip: 0,
            recipe: OpacityMicromapRecipe([recipe; 32]),
        }
    }

    struct FakeDerivedBaker {
        calls: usize,
        base: Geometry,
    }

    #[test]
    fn mesh_hook_visits_empty_and_skinned_meshes_without_candidates() {
        struct Baker(usize);

        impl DerivedAssetBaker for Baker {
            fn bake_mesh(&mut self, mesh: &mut Mesh) -> anyhow::Result<()> {
                assert_eq!(mesh.skin().is_some(), self.0 == 1);
                mesh.data
                    .insert("visited", crate::scene::DataData::Bool(true));
                self.0 += 1;
                Ok(())
            }

            fn bake(
                &mut self,
                _: &DerivedAssetBakeCandidate<'_>,
            ) -> anyhow::Result<Box<[DerivedAssetBakeOutput]>> {
                panic!("unexpected OMM candidate")
            }
        }

        let mut writer = Writer::default();
        writer
            .push_mesh(Mesh::new(Vec::new(), None).unwrap(), policy(None))
            .unwrap();
        writer
            .push_mesh(
                Mesh::new(
                    vec![
                        Primitive::new(
                            0,
                            Geometry::new(
                                &[0; 20],
                                VertexType::JOINTS_WEIGHTS,
                                IndexBuffer::new(&[0, 0, 0]).unwrap(),
                            )
                            .unwrap(),
                        )
                        .unwrap(),
                    ],
                    Some(Skin::new(vec![Joint {
                        inverse_bind: glam::Mat4::IDENTITY.to_cols_array(),
                        name: "root".to_owned(),
                        parent_index: 0,
                    }])),
                )
                .unwrap(),
                policy(Some(Compression::Snap)),
            )
            .unwrap();
        let mut baker = Baker(0);
        writer.bake_derived_assets(&mut baker).unwrap();
        assert_eq!(baker.0, 2);
        assert!(writer.mesh_spool.is_none());
        assert_eq!(writer.spool.as_ref().unwrap().entries[&0].len(), 2);
        for source in writer.data.meshes.clone() {
            let mesh: Mesh = writer.read_spooled(&source).unwrap();
            assert!(mesh.data("visited").unwrap().expect_bool());
            assert!(mesh.blob().is_none());
        }
    }

    impl DerivedAssetBaker for FakeDerivedBaker {
        fn bake(
            &mut self,
            candidate: &DerivedAssetBakeCandidate<'_>,
        ) -> anyhow::Result<Box<[DerivedAssetBakeOutput]>> {
            self.calls += 1;
            assert_eq!(candidate.mesh, crate::MeshId(0));
            assert_eq!(candidate.primitive, 0);
            assert_eq!(candidate.material, crate::MaterialId(0));
            assert_eq!(candidate.indices.as_ref(), self.base.indices().as_u32());
            assert_eq!(
                candidate.texture0.as_ref(),
                self.base.texture0().unwrap().collect::<Vec<_>>()
            );
            assert_eq!(candidate.alpha_bitmap.format(), BitmapCompression::Bc3);
            let triangles = candidate.indices.len() / 3;
            Ok(vec![DerivedAssetBakeOutput {
                payload: OpacityMicromap::new(
                    [],
                    [],
                    [],
                    vec![-1; triangles],
                    [],
                    triangles as u32,
                    0,
                )
                .unwrap(),
                recipe: OpacityMicromapRecipe([7; 32]),
                source_mip: 0,
            }]
            .into_boxed_slice())
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
    fn mesh_pack_defaults_preserve_raw_recipe_identity_and_dependency_policy() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        fs::copy(
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/data/scene/cube.glb"),
            root.join("cube.glb"),
        )
        .unwrap();
        fs::write(root.join("payload.bin"), b"mesh dependency").unwrap();
        let mut recipe: MeshAsset =
            toml::from_str("src = 'cube.glb'\nblob = 'payload.bin'").unwrap();
        recipe.canonicalize(root, root);
        let asset: Asset = recipe.clone().into();
        let direct = StoragePolicy {
            segment: 1,
            ..policy(Some(Compression::Snap))
        };
        let inherited = StoragePolicy {
            segment: 2,
            ..policy(None)
        };
        let writer = Arc::new(Mutex::new(Writer {
            default_lods: vec![
                crate::buf::LodRequest {
                    layout: crate::mesh::VertexType::POSITION,
                    simplify: true,
                },
                crate::buf::LodRequest {
                    layout: crate::mesh::VertexType::PACKED_NORMAL,
                    simplify: true,
                },
            ]
            .into_boxed_slice(),
            ..Writer::default()
        }));
        {
            let mut writer = writer.lock();
            writer
                .set_segments(vec!["meshdata".to_owned(), "outer".to_owned()])
                .unwrap();
            writer
                .register_direct_policy(asset.clone(), direct)
                .unwrap();
            writer.set_active_policy(inherited);
        }
        let id = recipe
            .bake(&writer, root, Some(root.join("mesh.toml")))
            .unwrap();
        assert_eq!(
            recipe
                .bake(&writer, root, Some(root.join("alias.toml")))
                .unwrap(),
            id
        );
        assert!(recipe.lod_requests().is_empty());
        let mut writer = writer.lock();
        assert_eq!(writer.ctx[&asset].id, id.into());
        assert_eq!(writer.ctx[&asset].policy, direct);
        assert_eq!(writer.ctx.len(), 2, "one raw mesh identity and its blob");
        assert_eq!(writer.data.ids["mesh"], writer.data.ids["alias"]);
        assert_eq!(writer.mesh_policies, [direct]);
        assert_eq!(writer.active_policy, inherited);
        let blob_range = writer.data.blobs[0].data_range().unwrap();
        assert_eq!(blob_range.segment, direct.segment);
        assert_eq!(blob_range.compression, direct.compression);
        let range = writer.data.meshes[id.0].data_range().unwrap();
        let mesh: Mesh = writer.mesh_spool.as_mut().unwrap().read(&range).unwrap();
        assert!(
            mesh.primitives()
                .iter()
                .all(|primitive| primitive.lod_sets().len() == 2)
        );
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
    fn opacity_micromap_payload_round_trips_independently() {
        let directory = tempfile::tempdir().unwrap();
        let destination = directory.path().join("opacity-micromap.pak");
        let mut writer = Writer::default();
        let bitmap = Bitmap::new(BitmapColor::Srgb, BitmapFormat::Rgba, 1, 1, [0; 4])
            .with_compressed(CompressedBitmap::new(
                BitmapCompression::Bc3,
                vec![CompressedMip::new(1, 1, vec![0; 16])],
            ));
        let color = writer.push_bitmap(bitmap, policy(None)).unwrap();
        writer.push_material(MaterialInfo {
            alpha_test: true,
            color,
            emissive: None,
            normal: None,
            params: None,
            params_used: MaterialParameterFlags::empty(),
            data: Default::default(),
        });
        writer
            .push_mesh(Mesh::new(Vec::new(), None).unwrap(), policy(None))
            .unwrap();
        let key = opacity_micromap_key(1);
        let expected = opacity_micromap();
        let id = writer
            .push_opacity_micromap(key, expected.clone(), policy(None))
            .unwrap();

        assert!(matches!(
            writer.data.opacity_micromaps[0],
            crate::DataRef::Ref(_)
        ));
        writer.write(&destination).unwrap();

        let mut pak = PakBuf::open(destination).unwrap();
        assert_eq!(pak.opacity_micromap_infos()[0].key, key);
        assert_eq!(pak.opacity_micromap_id(key), Some(id));
        assert_eq!(pak.read_opacity_micromap(key).unwrap(), Some(expected));
        assert!(
            pak.read_opacity_micromap(opacity_micromap_key(2))
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn derived_assets_use_final_scene_bindings_and_deduplicate_candidates() {
        let base = crate::mesh::test::grid(17, true);
        let directory = tempfile::tempdir().unwrap();
        let mut previous = None;
        for alternatives in [false, true] {
            let mut writer = Writer::default();
            let policy = policy(Some(Compression::Snap));
            let bitmap = Bitmap::new(BitmapColor::Srgb, BitmapFormat::Rgba, 1, 1, [0; 4])
                .with_compressed(CompressedBitmap::new(
                    BitmapCompression::Bc3,
                    vec![CompressedMip::new(1, 1, vec![0; 16])],
                ));
            let color = writer.push_bitmap(bitmap, policy).unwrap();
            let material = writer.push_material(MaterialInfo {
                alpha_test: true,
                color,
                emissive: None,
                normal: None,
                params: None,
                params_used: MaterialParameterFlags::empty(),
                data: Default::default(),
            });
            let mut primitive = Primitive::new(0, base.clone()).unwrap();
            if alternatives {
                let asset: MeshAsset =
                    toml::from_str("lods = [{layout='POSITION', simplify=true}, {layout='PACKED_NORMAL', simplify=true}]").unwrap();
                asset.generate_lods(&mut primitive).unwrap();
                assert!(
                    primitive
                        .lod_sets()
                        .iter()
                        .all(|set| set.levels().len() > 1)
                );
            }
            assert_eq!(primitive.base(), &base);
            let expected = primitive.clone();
            let mesh = writer
                .push_mesh(Mesh::new(vec![primitive], None).unwrap(), policy)
                .unwrap();
            let reference = ReferenceData {
                materials: vec![material],
                mesh: Some(mesh),
                ..Default::default()
            };
            writer
                .push_scene(
                    Scene::new(
                        [],
                        [
                            reference,
                            ReferenceData {
                                materials: vec![material],
                                mesh: Some(mesh),
                                ..Default::default()
                            },
                        ],
                    )
                    .unwrap(),
                    policy,
                )
                .unwrap();
            let mut baker = FakeDerivedBaker {
                calls: 0,
                base: base.clone(),
            };

            writer.bake_derived_assets(&mut baker).unwrap();

            assert_eq!(baker.calls, 1);
            assert_eq!(writer.data.opacity_micromaps.len(), 1);
            assert_eq!(writer.data.opacity_micromap_infos[0].key.recipe.0, [7; 32]);
            let destination = directory.path().join(format!("native-{alternatives}.pak"));
            writer.write(&destination).unwrap();
            let bytes = fs::read(&destination).unwrap();
            assert_eq!(&bytes[..20], b"ATTACKGOAT-PAK-V1.10");
            let mut pak = PakBuf::open(destination).unwrap();
            let decoded = pak.read_mesh_id(mesh).unwrap();
            assert_eq!(decoded.primitives(), &[expected]);
            let key = pak.opacity_micromap_infos()[0].key;
            let payload = pak.read_opacity_micromap(key).unwrap().unwrap();
            let output =
                bincode::serde::encode_to_vec((key, payload), bincode::config::legacy()).unwrap();
            if let Some(previous) = previous {
                assert_eq!(
                    output, previous,
                    "alternatives must not change base OMM output"
                );
            }
            previous = Some(output);
        }
    }

    #[test]
    fn opacity_micromap_keys_must_be_strictly_sorted() {
        let mut writer = Writer::default();
        let key = opacity_micromap_key(1);
        writer
            .push_opacity_micromap(key, opacity_micromap(), policy(None))
            .unwrap();

        assert_eq!(
            writer
                .push_opacity_micromap(key, opacity_micromap(), policy(None))
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
        assert_eq!(
            writer
                .push_opacity_micromap(opacity_micromap_key(0), opacity_micromap(), policy(None),)
                .unwrap_err()
                .kind(),
            ErrorKind::InvalidInput
        );
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
