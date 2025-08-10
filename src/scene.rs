use {
    super::{MaterialId, MeshId, Quat, Vec3, index::IndexBuffer},
    serde::{Deserialize, Serialize},
    std::collections::HashMap,
};

type StringIndex = u16;

#[derive(Clone, Debug, Deserialize, Serialize)]
enum Data {
    Array(Vec<Data>),
    Bool(bool),
    Float(f32),
    Number(i32),
    String(StringIndex),
}

impl Data {
    fn parse(value: DataData, st: &mut StringTable) -> Self {
        match value {
            DataData::Array(values) => Self::Array(
                values
                    .into_iter()
                    .map(|value| Self::parse(value, st))
                    .collect(),
            ),
            DataData::Bool(value) => Self::Bool(value),
            DataData::Float(value) => Self::Float(value),
            DataData::Number(value) => Self::Number(value),
            DataData::String(value) => Self::String(st.get(value)),
        }
    }
}

#[derive(Debug)]
struct DataIter<'a> {
    data: &'a [Data],
    idx: usize,
    scene: &'a Scene,
}

impl ExactSizeIterator for DataIter<'_> {
    fn len(&self) -> usize {
        self.data.len() - self.idx
    }
}

impl<'a> Iterator for DataIter<'a> {
    type Item = DataRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx < self.data.len() {
            let res = DataRef {
                scene: self.scene,
                data: &self.data[self.idx],
            };
            self.idx += 1;
            Some(res)
        } else {
            None
        }
    }
}

/// Encapsulates any scene data.
#[derive(Clone, Debug)]
pub enum DataData {
    Array(Vec<DataData>),
    Bool(bool),
    Float(f32),
    Number(i32),
    String(String),
}

/// An individual `Scene` data.
#[derive(Clone, Copy, Debug)]
pub struct DataRef<'a> {
    data: &'a Data,
    scene: &'a Scene,
}

impl<'a> DataRef<'a> {
    /// Returns a value if the data is a boolean.
    pub fn as_bool(self) -> Option<bool> {
        if let &Data::Bool(value) = self.data {
            Some(value)
        } else {
            None
        }
    }

    /// Returns a value if the data is a float.
    pub fn as_f32(self) -> Option<f32> {
        if let &Data::Float(value) = self.data {
            Some(value)
        } else {
            None
        }
    }

    /// Returns a value if the data is a number.
    pub fn as_i32(self) -> Option<i32> {
        if let &Data::Number(value) = self.data {
            Some(value)
        } else {
            None
        }
    }

    /// Returns an iterator if the data is an array.
    pub fn as_iter(self) -> Option<impl ExactSizeIterator<Item = DataRef<'a>> + 'a> {
        if let Data::Array(values) = self.data {
            Some(DataIter {
                data: values,
                idx: 0,
                scene: self.scene,
            })
        } else {
            None
        }
    }

    /// Returns a reference if the data is a string.
    pub fn as_str(self) -> Option<&'a str> {
        if let &Data::String(idx) = self.data {
            Some(self.scene.str(idx))
        } else {
            None
        }
    }

    fn as_type_str(self) -> &'static str {
        match self.data {
            Data::Array(_) => "iter",
            Data::Bool(_) => "bool",
            Data::Float(_) => "f32",
            Data::Number(_) => "i32",
            Data::String(_) => "str",
        }
    }

    /// Returns a boolean.
    pub fn expect_bool(self) -> bool {
        self.as_bool()
            .unwrap_or_else(|| panic!("expected bool, found {}", self.as_type_str()))
    }

    /// Returns a float.
    pub fn expect_f32(self) -> f32 {
        self.as_f32()
            .unwrap_or_else(|| panic!("expected f32, found {}", self.as_type_str()))
    }

    /// Returns a number.
    pub fn expect_i32(self) -> i32 {
        self.as_i32()
            .unwrap_or_else(|| panic!("expected i32, found {}", self.as_type_str()))
    }

    /// Returns an array.
    pub fn expect_iter(self) -> impl ExactSizeIterator<Item = DataRef<'a>> + 'a {
        self.as_iter()
            .unwrap_or_else(|| panic!("expected iter, found {}", self.as_type_str()))
    }

    /// Returns a string.
    pub fn expect_str(self) -> &'a str {
        self.as_str()
            .unwrap_or_else(|| panic!("expected str, found {}", self.as_type_str()))
    }

    /// Returns `true` if the data is a boolean.
    pub fn is_bool(self) -> bool {
        matches!(self.data, Data::Bool(_))
    }

    /// Returns `true` if the data is a float.
    pub fn is_f32(self) -> bool {
        matches!(self.data, Data::Float(_))
    }

    /// Returns `true` if the data is a number.
    pub fn is_i32(self) -> bool {
        matches!(self.data, Data::Number(_))
    }

    /// Returns `true` if the data is an array.
    pub fn is_iter(self) -> bool {
        matches!(self.data, Data::Array(_))
    }

    /// Returns `true` if the data is a string.
    pub fn is_str(self) -> bool {
        matches!(self.data, Data::String(_))
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Geometry {
    data: Box<[(StringIndex, Data)]>,
    id: Option<StringIndex>,
    index_buf: IndexBuffer,
    rotation: Quat,
    tags: Box<[StringIndex]>,
    translation: Vec3,

    #[serde(with = "serde_bytes")]
    vertex_buf: Box<[u8]>,
}

pub struct GeometryData {
    pub data: Vec<(String, DataData)>,
    pub id: Option<String>,
    pub indices: Vec<u32>,
    pub vertices: Vec<u8>,
    pub rotation: Quat,
    pub tags: Vec<String>,
    pub translation: Vec3,
}

/// An individual `Scene` geometry.
#[derive(Clone, Copy, Debug)]
pub struct GeometryRef<'a> {
    idx: usize,
    scene: &'a Scene,
}

impl GeometryRef<'_> {
    /// Returns the data for the given key, if it exists.
    pub fn data(&self, key: &str) -> Option<DataRef<'_>> {
        let geometry = self.geometry();

        match geometry
            .data
            .binary_search_by(|(probe, _)| self.scene.str(*probe).cmp(key))
        {
            Ok(idx) => Some(DataRef {
                data: &geometry.data[idx].1,
                scene: self.scene,
            }),
            Err(_) => None,
        }
    }

    /// Returns `true` if the geometry contains the given tag.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.geometry()
            .tags
            .binary_search_by(|probe| self.scene.str(*probe).cmp(tag))
            .is_ok()
    }

    /// Returns `id`, if set.
    pub fn id(&self) -> Option<&str> {
        self.geometry().id.map(|idx| self.scene.str(idx))
    }

    pub fn index_buf(&self) -> &IndexBuffer {
        &self.geometry().index_buf
    }

    /// Returns `translation` or the zero vector.
    pub fn translation(&self) -> Vec3 {
        self.geometry().translation
    }

    /// Returns `rotation` or the identity quaternion.
    pub fn rotation(&self) -> Quat {
        self.geometry().rotation
    }

    fn geometry(&self) -> &Geometry {
        &self.scene.geometries[self.idx]
    }

    /// Returns an `Iterator` of tags.
    pub fn tags(&self) -> impl ExactSizeIterator<Item = &str> {
        self.geometry()
            .tags
            .iter()
            .map(move |idx| self.scene.str(*idx))
    }

    pub fn vertex_data(&self) -> &[u8] {
        &self.geometry().vertex_buf
    }
}

/// An `Iterator` of [`Geometry`] items.
#[derive(Clone, Debug)]
struct GeometryIter<'a> {
    idx: usize,
    scene: &'a Scene,
}

impl ExactSizeIterator for GeometryIter<'_> {
    fn len(&self) -> usize {
        self.scene.geometries.len() - self.idx
    }
}

impl<'a> Iterator for GeometryIter<'a> {
    type Item = GeometryRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx < self.scene.geometries.len() {
            let res = GeometryRef {
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

#[derive(Clone, Debug, Deserialize, Serialize)]
struct Reference {
    data: Box<[(StringIndex, Data)]>,
    id: Option<StringIndex>,
    materials: Box<[MaterialId]>,
    mesh: Option<MeshId>,
    rotation: Quat,
    tags: Box<[StringIndex]>,
    translation: Vec3,
}

#[derive(Default)]
pub struct ReferenceData {
    pub data: Vec<(String, DataData)>,
    pub id: Option<String>,
    pub materials: Vec<MaterialId>,
    pub mesh: Option<MeshId>,
    pub rotation: Quat,
    pub tags: Vec<String>,
    pub translation: Vec3,
}

/// A container for scene entities.
#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Scene {
    geometries: Box<[Geometry]>,
    references: Box<[Reference]>,
    strs: Box<[String]>,
}

impl Scene {
    pub(crate) fn new(
        geometries: impl IntoIterator<Item = GeometryData>,
        references: impl IntoIterator<Item = ReferenceData>,
    ) -> Self {
        // Use a string table
        let mut st = StringTable::default();

        let geometries = geometries
            .into_iter()
            .map(|geometry| {
                let mut tags = geometry
                    .tags
                    .into_iter()
                    .map(|tag| st.get(tag))
                    .collect::<Box<_>>();
                tags.sort();

                let index_buf = IndexBuffer::new(&geometry.indices);

                Geometry {
                    data: geometry
                        .data
                        .into_iter()
                        .map(|(key, value)| (st.get(key), Data::parse(value, &mut st)))
                        .collect(),
                    id: geometry.id.map(|id| st.get(id)),
                    index_buf,
                    rotation: geometry.rotation,
                    tags,
                    translation: geometry.translation,
                    vertex_buf: geometry.vertices.into_boxed_slice(),
                }
            })
            .collect();

        let references = references
            .into_iter()
            .map(|reference| {
                let mut tags = reference
                    .tags
                    .into_iter()
                    .map(|tag| st.get(tag))
                    .collect::<Box<_>>();
                tags.sort();

                Reference {
                    data: reference
                        .data
                        .into_iter()
                        .map(|(key, value)| (st.get(key), Data::parse(value, &mut st)))
                        .collect(),
                    id: reference.id.map(|id| st.get(id)),
                    mesh: reference.mesh,
                    materials: reference.materials.into_boxed_slice(),
                    rotation: reference.rotation,
                    tags,
                    translation: reference.translation,
                }
            })
            .collect();

        Self {
            geometries,
            references,
            strs: st.strs.into_boxed_slice(),
        }
    }

    /// Gets an iterator of the `Geometry` items stored in this `Scene`.
    pub fn geometries(&self) -> impl ExactSizeIterator<Item = GeometryRef<'_>> {
        GeometryIter {
            idx: 0,
            scene: self,
        }
    }

    /// Gets an iterator of the `Reference` items stored in this `Scene`.
    pub fn refs(&self) -> impl ExactSizeIterator<Item = ReferenceRef<'_>> {
        ReferenceIter {
            idx: 0,
            scene: self,
        }
    }

    fn str(&self, idx: StringIndex) -> &str {
        self.strs[idx as usize].as_str()
    }
}

/// An individual `Scene` reference.
#[derive(Clone, Copy, Debug)]
pub struct ReferenceRef<'a> {
    idx: usize,
    scene: &'a Scene,
}

impl ReferenceRef<'_> {
    /// Returns the data for the given key, if it exists.
    pub fn data(&self, key: &str) -> Option<DataRef<'_>> {
        let reference = self.reference();

        match reference
            .data
            .binary_search_by(|(probe, _)| self.scene.str(*probe).cmp(key))
        {
            Ok(idx) => Some(DataRef {
                data: &reference.data[idx].1,
                scene: self.scene,
            }),
            Err(_) => None,
        }
    }

    /// Returns `true` if the ref contains the given tag.
    pub fn has_tag(&self, tag: &str) -> bool {
        self.reference()
            .tags
            .binary_search_by(|probe| self.scene.str(*probe).cmp(tag))
            .is_ok()
    }

    /// Returns `id`, if set.
    pub fn id(&self) -> Option<&str> {
        self.scene.references[self.idx]
            .id
            .map(|idx| self.scene.str(idx))
    }

    /// Returns `material`, if set.
    pub fn materials(&self) -> &[MaterialId] {
        &self.reference().materials
    }

    /// Returns `mesh`, if set.
    pub fn mesh(&self) -> Option<MeshId> {
        self.reference().mesh
    }

    /// Returns `translation` or the zero vector.
    pub fn translation(&self) -> Vec3 {
        self.reference().translation
    }

    /// Returns `rotation` or the identity quaternion.
    pub fn rotation(&self) -> Quat {
        self.reference().rotation
    }

    fn reference(&self) -> &Reference {
        &self.scene.references[self.idx]
    }

    /// Returns an `Iterator` of tags.
    pub fn tags(&self) -> impl ExactSizeIterator<Item = &str> {
        self.reference()
            .tags
            .iter()
            .map(move |idx| self.scene.str(*idx))
    }
}

/// An `Iterator` of [`Reference`] items.
#[derive(Clone, Debug)]
struct ReferenceIter<'a> {
    idx: usize,
    scene: &'a Scene,
}

impl ExactSizeIterator for ReferenceIter<'_> {
    fn len(&self) -> usize {
        self.scene.references.len() - self.idx
    }
}

impl<'a> Iterator for ReferenceIter<'a> {
    type Item = ReferenceRef<'a>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.idx < self.scene.references.len() {
            let res = ReferenceRef {
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

#[derive(Default)]
struct StringTable {
    cache: HashMap<String, StringIndex>,
    strs: Vec<String>,
}

impl StringTable {
    fn get(&mut self, s: String) -> StringIndex {
        *self.cache.entry(s.clone()).or_insert_with(|| {
            assert!(self.strs.len() < StringIndex::MAX as usize);

            let res = self.strs.len() as StringIndex;
            self.strs.push(s);

            res
        })
    }
}
