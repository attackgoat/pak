use {
    anyhow::Context as _,
    gltf::{Document, Gltf, buffer::Data, import_buffers},
    std::{
        path::{Path, PathBuf},
        sync::Arc,
    },
};

pub(super) struct MeshSource {
    pub(super) document: Document,
    pub(super) buffers: Box<[Data]>,
}

impl MeshSource {
    pub(super) fn load(path: &Path) -> anyhow::Result<Self> {
        let Gltf { document, blob } = Gltf::open(path)
            .with_context(|| format!("importing mesh source {}", path.display()))?;
        let base = path.parent().unwrap_or_else(|| Path::new("./"));
        // Mesh import reads geometry and material indices, never decoded image pixels.
        let buffers = import_buffers(&document, Some(base), blob)
            .with_context(|| format!("importing mesh buffers {}", path.display()))?
            .into_boxed_slice();
        Ok(Self { document, buffers })
    }
}

#[derive(Default)]
pub(super) struct MeshSourceCache {
    source: Option<(PathBuf, Arc<MeshSource>)>,
}

impl MeshSourceCache {
    pub(super) fn load(&mut self, path: &Path) -> anyhow::Result<Arc<MeshSource>> {
        if let Some((cached_path, source)) = &self.source
            && cached_path == path
        {
            return Ok(Arc::clone(source));
        }

        // Retain only the most recent file, and only for this Writer's bake lifetime.
        self.source = None;
        let source = Arc::new(MeshSource::load(path)?);
        self.source = Some((path.to_path_buf(), Arc::clone(&source)));
        Ok(source)
    }
}

#[cfg(test)]
mod test {
    use {
        super::{MeshSource, MeshSourceCache},
        gltf::binary::{Glb, Header},
        serde_json::{Value, json},
        std::{borrow::Cow, fs, path::Path, sync::Arc},
    };

    fn write_gltf(root: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = root.join(format!("{name}.gltf"));
        fs::write(root.join(format!("{name}.bin")), bytes).unwrap();
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "asset": {"version": "2.0"},
                "buffers": [{"uri": format!("{name}.bin"), "byteLength": bytes.len()}],
            }))
            .unwrap(),
        )
        .unwrap();
        path
    }

    #[test]
    fn external_geometry_does_not_read_or_decode_images() {
        let directory = tempfile::tempdir().unwrap();
        let root = directory.path();
        let bytes = [1, 2, 3, 4];
        let path = write_gltf(root, "mesh", &bytes);
        let mut document: Value = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        document["images"] = json!([
            {"uri": "missing.png"},
            {"uri": "invalid.png"},
        ]);
        fs::write(root.join("invalid.png"), b"not a png").unwrap();
        fs::write(&path, serde_json::to_vec(&document).unwrap()).unwrap();
        assert!(gltf::import(&path).is_err());

        let source = MeshSource::load(&path).unwrap();
        assert_eq!(source.document.images().len(), 2);
        assert_eq!(source.buffers.len(), 1);
        assert_eq!(source.buffers[0].0, bytes);
    }

    #[test]
    fn embedded_geometry_does_not_decode_images() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mesh.glb");
        let bytes = [1, 2, 3, 4];
        let glb = Glb {
            header: Header {
                magic: *b"glTF",
                version: 2,
                length: 0,
            },
            json: Cow::Owned(
                serde_json::to_vec(&json!({
                    "asset": {"version": "2.0"},
                    "buffers": [{"byteLength": bytes.len()}],
                    "bufferViews": [{"buffer": 0, "byteLength": bytes.len()}],
                    "images": [{"bufferView": 0, "mimeType": "image/png"}],
                }))
                .unwrap(),
            ),
            bin: Some(Cow::Borrowed(&bytes)),
        };
        fs::write(&path, glb.to_vec().unwrap()).unwrap();
        assert!(gltf::import(&path).is_err());

        let source = MeshSource::load(&path).unwrap();
        assert_eq!(source.document.images().len(), 1);
        assert_eq!(source.buffers[0].0, bytes);
    }

    #[test]
    fn data_uri_buffers_still_import() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("mesh.gltf");
        fs::write(
            &path,
            serde_json::to_vec(&json!({
                "asset": {"version": "2.0"},
                "buffers": [{
                    "uri": "data:application/octet-stream;base64,AQIDBA==",
                    "byteLength": 4,
                }],
            }))
            .unwrap(),
        )
        .unwrap();
        assert_eq!(MeshSource::load(&path).unwrap().buffers[0].0, [1, 2, 3, 4]);
    }

    #[test]
    fn short_geometry_buffers_are_rejected() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_gltf(directory.path(), "mesh", &[1, 2, 3, 4]);
        fs::write(directory.path().join("mesh.bin"), []).unwrap();
        let error = MeshSource::load(&path).err().unwrap();
        assert!(format!("{error:#}").contains("buffer"));
    }

    #[test]
    fn cache_reuses_the_last_source_and_releases_previous_files() {
        let directory = tempfile::tempdir().unwrap();
        let first = write_gltf(directory.path(), "first", &[1, 2, 3, 4]);
        let second = write_gltf(directory.path(), "second", &[5, 6, 7, 8]);
        let mut cache = MeshSourceCache::default();
        let source = cache.load(&first).unwrap();
        let previous = Arc::downgrade(&source);
        fs::remove_file(&first).unwrap();
        assert!(Arc::ptr_eq(&source, &cache.load(&first).unwrap()));
        drop(source);

        assert_eq!(cache.load(&second).unwrap().buffers[0].0, [5, 6, 7, 8]);
        assert!(previous.upgrade().is_none());
        assert!(cache.load(&first).is_err());
    }

    #[test]
    fn separate_caches_read_current_source_buffers() {
        let directory = tempfile::tempdir().unwrap();
        let path = write_gltf(directory.path(), "mesh", &[1, 2, 3, 4]);
        let mut first = MeshSourceCache::default();
        let original = first.load(&path).unwrap();
        fs::write(directory.path().join("mesh.bin"), [5, 6, 7, 8]).unwrap();
        let current = MeshSourceCache::default().load(&path).unwrap();
        assert_eq!(original.buffers[0].0, [1, 2, 3, 4]);
        assert_eq!(current.buffers[0].0, [5, 6, 7, 8]);
        assert!(!Arc::ptr_eq(&original, &current));
    }
}
