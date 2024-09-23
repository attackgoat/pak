use {
    super::{super::compression::Compression, Asset},
    crate::{
        anim::Animation, bitmap::Bitmap, bitmap_font::BitmapFont, model::Model, scene::Scene,
        AnimationId, BitmapFontId, BitmapId, BlobId, Data, DataRef, Id, MaterialId, MaterialInfo,
        ModelId, SceneId,
    },
    log::trace,
    serde::Serialize,
    std::{
        collections::HashMap,
        fs::File,
        io::{BufWriter, Error, ErrorKind, Seek, SeekFrom, Write},
        path::Path,
    },
};

#[derive(Default)]
pub struct Writer {
    compression: Option<Compression>,
    pub(super) ctx: HashMap<Asset, Id>,
    data: Data,
}

impl Writer {
    pub fn push_animation(&mut self, animation: Animation, key: Option<String>) -> AnimationId {
        let id = AnimationId(self.data.anims.len());
        self.data.anims.push(DataRef::Data(animation));

        if let Some(key) = key {
            assert!(self.data.ids.get(&key).is_none());

            self.data.ids.insert(key, id.into());
        }

        id
    }

    pub fn push_bitmap_font(
        &mut self,
        bitmap_font: BitmapFont,
        key: Option<String>,
    ) -> BitmapFontId {
        let id = BitmapFontId(self.data.bitmap_fonts.len());
        self.data.bitmap_fonts.push(DataRef::Data(bitmap_font));

        if let Some(key) = key {
            assert!(self.data.ids.get(&key).is_none());

            self.data.ids.insert(key, id.into());
        }

        id
    }

    pub fn push_bitmap(&mut self, bitmap: Bitmap, key: Option<String>) -> BitmapId {
        let id = BitmapId(self.data.bitmaps.len());
        self.data.bitmaps.push(DataRef::Data(bitmap));

        if let Some(key) = key {
            assert!(self.data.ids.get(&key).is_none());

            self.data.ids.insert(key, id.into());
        }

        id
    }

    pub fn push_blob(&mut self, blob: Vec<u8>, key: Option<String>) -> BlobId {
        let id = BlobId(self.data.blobs.len());
        self.data.blobs.push(DataRef::Data(blob));

        if let Some(key) = key {
            assert!(self.data.ids.get(&key).is_none());

            self.data.ids.insert(key, id.into());
        }

        id
    }

    pub fn push_material(&mut self, info: MaterialInfo, key: Option<String>) -> MaterialId {
        let id = MaterialId(self.data.materials.len());
        self.data.materials.push(info);

        if let Some(key) = key {
            assert!(self.data.ids.get(&key).is_none());

            self.data.ids.insert(key, id.into());
        }

        id
    }

    pub fn push_model(&mut self, model: Model, key: Option<String>) -> ModelId {
        let id = ModelId(self.data.models.len());
        self.data.models.push(DataRef::Data(model));

        if let Some(key) = key {
            assert!(self.data.ids.get(&key).is_none());

            self.data.ids.insert(key, id.into());
        }

        id
    }

    pub fn push_scene(&mut self, scene: Scene, key: String) -> SceneId {
        let id = SceneId(self.data.scenes.len());
        self.data.scenes.push(DataRef::Data(scene));

        assert!(self.data.ids.get(&key).is_none());

        self.data.ids.insert(key, id.into());

        id
    }

    pub fn with_compression(&mut self, compression: Compression) -> &mut Self {
        self.compression = Some(compression);
        self
    }

    pub fn with_compression_is(&mut self, compression: Option<Compression>) -> &mut Self {
        self.compression = compression;
        self
    }

    pub fn write(&mut self, path: impl AsRef<Path>) -> Result<(), Error> {
        self.write_data(&mut BufWriter::new(File::create(path)?))
    }

    fn write_data(&mut self, mut writer: impl Write + Seek) -> Result<(), Error> {
        let mut magic_bytes = [0u8; 20];
        magic_bytes.copy_from_slice(b"ATTACKGOAT-PAK-V1.0 ");

        // Write a known value so we can identify this file
        bincode::serialize_into(&mut writer, &magic_bytes)
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;

        let skip_position = writer.stream_position()?;

        // Write a blank spot that we'll use for the skip header later
        bincode::serialize_into(&mut writer, &0u32)
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;

        // Write the compression we're going to be using, if any
        bincode::serialize_into(&mut writer, &self.compression)
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;

        // Update these items with the refs we created; saving with bincode was very
        // slow when serializing the byte vectors - that is why those are saved raw.
        trace!(
            "Writing {} animation{}",
            self.data.anims.len(),
            if self.data.anims.len() == 1 { "" } else { "s" }
        );
        Self::write_refs(self.compression, &mut writer, &mut self.data.anims)?;

        trace!(
            "Writing {} bitmap{}",
            self.data.bitmaps.len(),
            if self.data.bitmaps.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        Self::write_refs(self.compression, &mut writer, &mut self.data.bitmaps)?;

        trace!(
            "Writing {} blob{}",
            self.data.blobs.len(),
            if self.data.blobs.len() == 1 { "" } else { "s" }
        );
        Self::write_refs(self.compression, &mut writer, &mut self.data.blobs)?;

        trace!(
            "Writing {} bitmap font{}",
            self.data.bitmap_fonts.len(),
            if self.data.bitmap_fonts.len() == 1 {
                ""
            } else {
                "s"
            }
        );
        Self::write_refs(self.compression, &mut writer, &mut self.data.bitmap_fonts)?;

        trace!(
            "Writing {} model{}",
            self.data.models.len(),
            if self.data.models.len() == 1 { "" } else { "s" }
        );
        Self::write_refs(self.compression, &mut writer, &mut self.data.models)?;

        trace!(
            "Writing {} scene{}",
            self.data.scenes.len(),
            if self.data.scenes.len() == 1 { "" } else { "s" }
        );
        Self::write_refs(self.compression, &mut writer, &mut self.data.scenes)?;

        // Write the data portion and then re-seek to the beginning to write the skip header
        let skip = writer.stream_position()? as u32;
        {
            let compressed = if let Some(compressed) = self.compression {
                compressed.new_writer(&mut writer)
            } else {
                Box::new(&mut writer)
            };
            bincode::serialize_into(compressed, &self.data)
                .map_err(|_| Error::from(ErrorKind::InvalidData))?;
        }

        writer.seek(SeekFrom::Start(skip_position))?;
        bincode::serialize_into(&mut writer, &skip)
            .map_err(|_| Error::from(ErrorKind::InvalidData))?;

        Ok(())
    }

    fn write_refs<T>(
        compression: Option<Compression>,
        mut writer: impl Seek + Write,
        refs: &mut Vec<DataRef<T>>,
    ) -> Result<(), Error>
    where
        T: Serialize,
    {
        let mut res = vec![];
        let mut start = writer.stream_position()? as _;

        for (idx, data) in refs.drain(..).map(|data| data.serialize()).enumerate() {
            // Write this data, compressed
            {
                let data = data?;
                let mut compressed = if let Some(compressed) = compression {
                    compressed.new_writer(&mut writer)
                } else {
                    Box::new(&mut writer)
                };
                compressed.write_all(&data)?;
            }

            // Push a ref
            let end = writer.stream_position()? as _;

            trace!("Index {idx} = {} bytes ({start}..{end})", end - start);

            res.push(DataRef::<T>::Ref(start..end));
            start = end;
        }

        *refs = res;

        Ok(())
    }
}
