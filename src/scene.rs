use {
    super::{MaterialId, MeshId, Quat, Vec3, index::IndexBuffer},
    serde::{Deserialize, Serialize},
    std::collections::{BTreeMap, HashMap},
};

type StringIndex = u16;

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
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
    strs: &'a [String],
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
                strs: self.strs,
                data: &self.data[self.idx],
            };
            self.idx += 1;
            Some(res)
        } else {
            None
        }
    }
}

/// Encapsulates application-defined scene, material, or mesh data.
#[derive(Clone, Debug)]
pub enum DataData {
    Array(Vec<DataData>),
    Bool(bool),
    Float(f32),
    Number(i32),
    String(String),
}

/// An owned collection of application-defined data with shared string storage.
#[derive(Clone, Debug, Default, PartialEq, Serialize)]
pub struct DataMap {
    data: Box<[(StringIndex, Data)]>,
    strs: Box<[String]>,
}

impl DataMap {
    /// Inserts or replaces a value, preserving other entries.
    ///
    /// Panics if the resulting map exceeds 65,535 shared strings.
    pub fn insert(&mut self, key: impl Into<String>, value: DataData) {
        *self = self
            .iter()
            .map(|(key, value)| (key.to_owned(), value.to_owned()))
            .chain([(key.into(), value)])
            .collect();
    }

    /// Returns the data for the given key, if it exists.
    pub fn get(&self, key: &str) -> Option<DataRef<'_>> {
        let idx = self
            .data
            .binary_search_by(|(idx, _)| self.strs[*idx as usize].as_str().cmp(key))
            .ok()?;
        Some(DataRef {
            data: &self.data[idx].1,
            strs: &self.strs,
        })
    }

    /// Iterates over entries in key order.
    pub fn iter(&self) -> impl Iterator<Item = (&str, DataRef<'_>)> {
        self.data.iter().map(|(key, data)| {
            (
                self.strs[*key as usize].as_str(),
                DataRef {
                    data,
                    strs: &self.strs,
                },
            )
        })
    }
}

impl<'de> Deserialize<'de> for DataMap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename = "DataMap")]
        struct SerializedDataMap {
            data: Box<[(StringIndex, Data)]>,
            strs: Box<[String]>,
        }

        let map = SerializedDataMap::deserialize(deserializer)?;

        #[cfg(debug_assertions)]
        {
            fn valid_strings(data: &Data, len: usize) -> bool {
                match data {
                    Data::Array(values) => values.iter().all(|value| valid_strings(value, len)),
                    Data::String(idx) => (*idx as usize) < len,
                    _ => true,
                }
            }

            assert!(
                map.data.iter().all(|(key, value)| {
                    (*key as usize) < map.strs.len() && valid_strings(value, map.strs.len())
                }),
                "DataMap string index is out of bounds"
            );
            assert!(
                map.data.windows(2).all(|entries| {
                    map.strs[entries[0].0 as usize] < map.strs[entries[1].0 as usize]
                }),
                "DataMap keys must be strictly sorted and unique"
            );
        }

        Ok(Self {
            data: map.data,
            strs: map.strs,
        })
    }
}

impl FromIterator<(String, DataData)> for DataMap {
    /// Builds a map in key order; the last value for a duplicate key wins.
    ///
    /// Panics if the map exceeds the shared string table's capacity (65,535 strings).
    fn from_iter<T: IntoIterator<Item = (String, DataData)>>(iter: T) -> Self {
        let mut st = StringTable::default();
        // Sort before interning so equality and serialization do not depend on input order.
        let values = iter.into_iter().collect::<BTreeMap<_, _>>();
        let data = values
            .into_iter()
            .map(|(key, value)| (st.get(key), Data::parse(value, &mut st)))
            .collect();
        Self {
            data,
            strs: st.strs.into_boxed_slice(),
        }
    }
}

/// An individual scene, material, or mesh data value.
#[derive(Clone, Copy, Debug)]
pub struct DataRef<'a> {
    data: &'a Data,
    strs: &'a [String],
}

impl<'a> DataRef<'a> {
    /// Copies this value, including nested arrays and strings, into owned storage.
    pub fn to_owned(self) -> DataData {
        match self.data {
            Data::Array(_) => {
                DataData::Array(self.as_iter().unwrap().map(Self::to_owned).collect())
            }
            Data::Bool(value) => DataData::Bool(*value),
            Data::Float(value) => DataData::Float(*value),
            Data::Number(value) => DataData::Number(*value),
            Data::String(_) => DataData::String(self.as_str().unwrap().to_owned()),
        }
    }

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
                strs: self.strs,
            })
        } else {
            None
        }
    }

    /// Returns a reference if the data is a string.
    pub fn as_str(self) -> Option<&'a str> {
        if let &Data::String(idx) = self.data {
            Some(
                self.strs
                    .get(idx as usize)
                    .map(String::as_str)
                    .unwrap_or_default(),
            )
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
                strs: &self.scene.strs,
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
    #[cfg(feature = "bake")]
    pub(crate) fn new(
        geometries: impl IntoIterator<Item = GeometryData>,
        references: impl IntoIterator<Item = ReferenceData>,
    ) -> anyhow::Result<Self> {
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
                tags.sort_by(|a, b| st.str(*a).cmp(st.str(*b)));

                let mut data = geometry
                    .data
                    .into_iter()
                    .map(|(key, value)| (st.get(key), Data::parse(value, &mut st)))
                    .collect::<Box<_>>();
                data.sort_by(|(a, _), (b, _)| st.str(*a).cmp(st.str(*b)));

                let index_buf = IndexBuffer::new(&geometry.indices)?;

                anyhow::Ok(Geometry {
                    data,
                    id: geometry.id.map(|id| st.get(id)),
                    index_buf,
                    rotation: geometry.rotation,
                    tags,
                    translation: geometry.translation,
                    vertex_buf: geometry.vertices.into_boxed_slice(),
                })
            })
            .collect::<anyhow::Result<Box<_>>>()?;

        let references = references
            .into_iter()
            .map(|reference| {
                let mut tags = reference
                    .tags
                    .into_iter()
                    .map(|tag| st.get(tag))
                    .collect::<Box<_>>();
                tags.sort_by(|a, b| st.str(*a).cmp(st.str(*b)));

                let mut data = reference
                    .data
                    .into_iter()
                    .map(|(key, value)| (st.get(key), Data::parse(value, &mut st)))
                    .collect::<Box<_>>();
                data.sort_by(|(a, _), (b, _)| st.str(*a).cmp(st.str(*b)));

                Reference {
                    data,
                    id: reference.id.map(|id| st.get(id)),
                    mesh: reference.mesh,
                    materials: reference.materials.into_boxed_slice(),
                    rotation: reference.rotation,
                    tags,
                    translation: reference.translation,
                }
            })
            .collect();

        Ok(Self {
            geometries,
            references,
            strs: st.strs.into_boxed_slice(),
        })
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
        self.strs
            .get(idx as usize)
            .map(String::as_str)
            .unwrap_or_default()
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
                strs: &self.scene.strs,
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

    #[cfg(feature = "bake")]
    fn str(&self, idx: StringIndex) -> &str {
        self.strs
            .get(idx as usize)
            .map(String::as_str)
            .unwrap_or_default()
    }
}

#[cfg(all(test, debug_assertions))]
mod data_map_test {
    use super::*;

    #[test]
    fn deserialization_asserts_metadata_invariants() {
        for map in [
            DataMap {
                data: Box::new([(0, Data::Bool(true))]),
                strs: Box::new([]),
            },
            DataMap {
                data: Box::new([(0, Data::Array(vec![Data::String(1)]))]),
                strs: Box::new(["key".to_owned()]),
            },
            DataMap {
                data: Box::new([(0, Data::Bool(true)), (1, Data::Bool(false))]),
                strs: Box::new(["z".to_owned(), "a".to_owned()]),
            },
            DataMap {
                data: Box::new([(0, Data::Bool(true)), (1, Data::Bool(false))]),
                strs: Box::new(["key".to_owned(), "key".to_owned()]),
            },
        ] {
            let bytes = bincode::serde::encode_to_vec(&map, bincode::config::legacy()).unwrap();
            assert!(
                std::panic::catch_unwind(|| {
                    bincode::serde::decode_from_slice::<DataMap, _>(
                        &bytes,
                        bincode::config::legacy(),
                    )
                })
                .is_err()
            );
        }
    }
}

#[cfg(all(test, feature = "bake"))]
mod test {
    use super::*;

    #[test]
    fn reference_lookup_does_not_depend_on_string_index_order() {
        let scene = Scene::new(
            [],
            [ReferenceData {
                data: vec![
                    ("z".to_owned(), DataData::Number(1)),
                    ("a".to_owned(), DataData::Number(2)),
                ],
                tags: vec!["z".to_owned(), "a".to_owned()],
                ..Default::default()
            }],
        )
        .unwrap();
        let reference = scene.refs().next().unwrap();

        assert_eq!(reference.data("z").unwrap().as_i32(), Some(1));
        assert_eq!(reference.data("a").unwrap().as_i32(), Some(2));
        assert!(reference.has_tag("z"));
        assert!(reference.has_tag("a"));
    }

    #[test]
    fn geometry_lookup_does_not_depend_on_string_index_order() {
        let scene = Scene::new(
            [GeometryData {
                data: vec![
                    ("z".to_owned(), DataData::Number(1)),
                    ("a".to_owned(), DataData::Number(2)),
                ],
                id: None,
                indices: vec![0, 1, 2],
                vertices: vec![],
                rotation: [0.0, 0.0, 0.0, 1.0],
                tags: vec!["z".to_owned(), "a".to_owned()],
                translation: [0.0, 0.0, 0.0],
            }],
            [],
        )
        .unwrap();
        let geometry = scene.geometries().next().unwrap();

        assert_eq!(geometry.data("z").unwrap().as_i32(), Some(1));
        assert_eq!(geometry.data("a").unwrap().as_i32(), Some(2));
        assert!(geometry.has_tag("z"));
        assert!(geometry.has_tag("a"));
    }
}
