use {
    super::{index::IndexBuffer, MaterialId, ModelId},
    glam::{Quat, Vec3},
    serde::{Deserialize, Serialize},
    std::collections::HashMap,
};

type Idx = u16;

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Geometry {
    id: Option<Idx>,
    index_buf: IndexBuffer,
    position: Vec3,
    rotation: Quat,
    tags: Vec<Idx>,

    #[serde(with = "serde_bytes")]
    vertex_buf: Vec<u8>,
}

pub struct GeometryData {
    pub id: Option<String>,
    pub indices: Vec<u32>,
    pub vertices: Vec<u8>,
    pub position: Vec3,
    pub rotation: Quat,
    pub tags: Vec<String>,
}

/// A container for scene entities.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct SceneBuf {
    geometries: Vec<Geometry>,
    refs: Vec<SceneRef>,
    strs: Vec<String>,
}

impl SceneBuf {
    pub(crate) fn new<G, R>(geometry_data: G, ref_data: R) -> Self
    where
        G: Iterator<Item = GeometryData>,
        R: Iterator<Item = SceneRefData>,
    {
        let mut strs = vec![];

        // Use a string table
        let mut cache = HashMap::new();
        let mut idx = |s: String| -> Idx {
            *cache.entry(s.clone()).or_insert_with(|| {
                let res = strs.len() as Idx;
                strs.push(s);
                res
            })
        };

        let mut geometries = vec![];
        for mut data in geometry_data {
            geometries.push(Geometry {
                id: data.id.map(&mut idx),
                index_buf: IndexBuffer::new(&data.indices),
                position: data.position,
                rotation: data.rotation,
                vertex_buf: data.vertices,
                tags: data.tags.drain(..).map(&mut idx).collect(),
            });
        }

        let mut refs = vec![];
        for mut data in ref_data {
            refs.push(SceneRef {
                id: data.id.map(&mut idx),
                model: data.model,
                material: data.material,
                position: data.position,
                rotation: data.rotation,
                tags: data.tags.drain(..).map(&mut idx).collect(),
            });
        }

        Self {
            geometries,
            refs,
            strs,
        }
    }

    /// Gets an iterator of the `Geometry` items stored in this `Scene`.
    pub fn geometries(&self) -> impl Iterator<Item = SceneBufGeometry<'_>> {
        SceneBufGeometryIter {
            idx: 0,
            scene: self,
        }
    }

    /// Gets an iterator of the `Ref` items stored in this `Scene`.
    pub fn refs(&self) -> impl Iterator<Item = SceneBufRef<'_>> {
        SceneBufRefIter {
            idx: 0,
            scene: self,
        }
    }

    fn scene_str<I: Into<usize>>(&self, idx: I) -> &str {
        self.strs[idx.into()].as_str()
    }
}

/// An individual `Scene` geometry.
#[derive(Debug)]
pub struct SceneBufGeometry<'a> {
    idx: usize,
    scene: &'a SceneBuf,
}

impl SceneBufGeometry<'_> {
    /// Returns `true` if the geometry contains the given tag.
    pub fn has_tag<T: AsRef<str>>(&self, tag: T) -> bool {
        let tag = tag.as_ref();
        self.geometry()
            .tags
            .binary_search_by(|probe| self.scene.scene_str(*probe).cmp(tag))
            .is_ok()
    }

    /// Returns `id`, if set.
    pub fn id(&self) -> Option<&str> {
        self.scene.refs[self.idx]
            .id
            .map(|idx| self.scene.strs[idx as usize].as_str())
    }

    pub fn index_buf(&self) -> &IndexBuffer {
        &self.geometry().index_buf
    }

    /// Returns `position` or the zero vector.
    pub fn position(&self) -> Vec3 {
        self.geometry().position
    }

    /// Returns `rotation` or the identity quaternion.
    pub fn rotation(&self) -> Quat {
        self.geometry().rotation
    }

    fn geometry(&self) -> &Geometry {
        &self.scene.geometries[self.idx]
    }

    /// Returns an `Iterator` of tags.
    pub fn tags(&self) -> impl Iterator<Item = &str> {
        self.geometry()
            .tags
            .iter()
            .map(move |idx| self.scene.scene_str(*idx))
    }

    pub fn vertex_data(&self) -> &[u8] {
        &self.geometry().vertex_buf
    }
}

/// An `Iterator` of [`Geometry`] items.
#[derive(Debug)]
struct SceneBufGeometryIter<'a> {
    idx: usize,
    scene: &'a SceneBuf,
}

impl<'a> Iterator for SceneBufGeometryIter<'a> {
    type Item = SceneBufGeometry<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx < self.scene.refs.len() {
            let res = SceneBufGeometry {
                scene: self.scene,
                idx: self.idx,
            };
            self.idx += 1;
            Some(res)
        } else {
            None
        }
    }
}

/// An individual `Scene` reference.
#[derive(Debug)]
pub struct SceneBufRef<'a> {
    idx: usize,
    scene: &'a SceneBuf,
}

impl SceneBufRef<'_> {
    /// Returns `true` if the ref contains the given tag.
    pub fn has_tag<T: AsRef<str>>(&self, tag: T) -> bool {
        let tag = tag.as_ref();
        self.scene_ref()
            .tags
            .binary_search_by(|probe| self.scene.scene_str(*probe).cmp(tag))
            .is_ok()
    }

    /// Returns `id`, if set.
    pub fn id(&self) -> Option<&str> {
        self.scene.refs[self.idx]
            .id
            .map(|idx| self.scene.strs[idx as usize].as_str())
    }

    /// Returns `material`, if set.
    pub fn material(&self) -> Option<MaterialId> {
        self.scene_ref().material
    }

    /// Returns `model`, if set.
    pub fn model(&self) -> Option<ModelId> {
        self.scene_ref().model
    }

    /// Returns `position` or the zero vector.
    pub fn position(&self) -> Vec3 {
        self.scene_ref().position
    }

    /// Returns `rotation` or the identity quaternion.
    pub fn rotation(&self) -> Quat {
        self.scene_ref().rotation
    }

    fn scene_ref(&self) -> &SceneRef {
        &self.scene.refs[self.idx]
    }

    /// Returns an `Iterator` of tags.
    pub fn tags(&self) -> impl Iterator<Item = &str> {
        self.scene_ref()
            .tags
            .iter()
            .map(move |idx| self.scene.scene_str(*idx))
    }
}

/// An `Iterator` of [`Ref`] items.
#[derive(Debug)]
struct SceneBufRefIter<'a> {
    idx: usize,
    scene: &'a SceneBuf,
}

impl<'a> Iterator for SceneBufRefIter<'a> {
    type Item = SceneBufRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx < self.scene.refs.len() {
            let res = SceneBufRef {
                scene: self.scene,
                idx: self.idx,
            };
            self.idx += 1;
            Some(res)
        } else {
            None
        }
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
struct SceneRef {
    id: Option<Idx>,
    material: Option<MaterialId>,
    model: Option<ModelId>,
    position: Vec3,
    rotation: Quat,
    tags: Vec<Idx>,
}

#[derive(Default)]
pub struct SceneRefData {
    pub id: Option<String>,
    pub material: Option<MaterialId>,
    pub model: Option<ModelId>,
    pub position: Vec3,
    pub rotation: Quat,
    pub tags: Vec<String>,
}
