use {
    super::{
        Asset, Canonicalize, Euler, Rotation, Writer, file_key, is_toml, material::MaterialAsset,
        mesh::MeshAsset, parent,
    },
    crate::{
        SceneId,
        scene::{DataData, GeometryData, ReferenceData, Scene},
    },
    anyhow::Context,
    glam::{EulerRot, Quat, Vec3, vec3},
    log::info,
    ordered_float::OrderedFloat,
    parking_lot::Mutex,
    serde::{
        Deserialize, Deserializer,
        de::{Error, MapAccess, Visitor, value::MapAccessDeserializer},
    },
    std::{
        collections::BTreeMap,
        fmt::Formatter,
        marker::PhantomData,
        mem::size_of,
        path::{Path, PathBuf},
        sync::Arc,
    },
    tokio::runtime::Runtime,
};

/// A reference to an asset or source file.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum AssetRef<T> {
    /// A `T` asset specified inline.
    Asset(T),

    /// A `T` asset file or `T` source file.
    Path(PathBuf),
}

impl<'de, T> AssetRef<T>
where
    T: Deserialize<'de>,
{
    /// Deserialize from any of absent or:
    ///
    /// src of file.gltf:
    /// .. = "file.gltf"
    ///
    /// src of file.toml which must be a `T` asset:
    /// .. = "file.toml"
    ///
    /// src of a `T` asset:
    /// .. = { src = "file.gltf" }
    fn de<D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct AssetRefVisitor<T>(PhantomData<T>);

        impl<'de, T> Visitor<'de> for AssetRefVisitor<T>
        where
            T: Deserialize<'de>,
        {
            type Value = Option<AssetRef<T>>;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("path string or asset")
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let asset = Deserialize::deserialize(MapAccessDeserializer::new(map))?;

                Ok(Some(AssetRef::Asset(asset)))
            }

            fn visit_str<E>(self, str: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(Some(AssetRef::Path(PathBuf::from(str))))
            }
        }

        deserializer.deserialize_any(AssetRefVisitor(PhantomData))
    }
}

impl<'de, T> Deserialize<'de> for AssetRef<T>
where
    T: Deserialize<'de>,
{
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        AssetRef::<T>::de(deserializer).transpose().unwrap()
    }
}

impl<T> Canonicalize for AssetRef<T>
where
    T: Canonicalize,
{
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        match self {
            Self::Asset(asset) => asset.canonicalize(project_dir, src_dir),
            Self::Path(src) => *src = Self::canonicalize_project_path(project_dir, src_dir, &src),
        }
    }
}

/// Holds a description of indexed triangle geometries.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Geometry {
    id: Option<String>,

    // Values
    euler: Option<Euler>,
    indices: Box<[u32]>,
    rotation: Option<Rotation>,
    translation: Option<[OrderedFloat<f32>; 3]>,
    vertices: Box<[OrderedFloat<f32>]>,

    // Tables must follow values
    tags: Option<Box<[String]>>,
    data: Option<BTreeMap<String, Data>>,
}

impl Geometry {
    /// An arbitrary collection of program-specific strings.
    #[allow(unused)]
    pub fn data(&self) -> impl Iterator<Item = (&String, &Data)> {
        self.data
            .as_ref()
            .map(|data| data.iter())
            .unwrap_or_default()
    }

    /// Euler ordering of the mesh orientation.
    pub fn euler(&self) -> EulerRot {
        match self.euler.unwrap_or(Euler::XYZ) {
            Euler::XYZ => EulerRot::XYZ,
            Euler::XZY => EulerRot::XZY,
            Euler::YXZ => EulerRot::YXZ,
            Euler::YZX => EulerRot::YZX,
            Euler::ZXY => EulerRot::ZXY,
            Euler::ZYX => EulerRot::ZYX,
        }
    }

    /// Main identifier of a geometry, not required to be unique.
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Orientation of a geometry.
    pub fn rotation(&self) -> Quat {
        match self.rotation {
            Some(Rotation::Euler(rotation)) => Quat::from_euler(
                self.euler(),
                rotation[0].0.to_radians(),
                rotation[1].0.to_radians(),
                rotation[2].0.to_radians(),
            ),
            Some(Rotation::Quaternion(rotation)) => {
                Quat::from_array([rotation[0].0, rotation[1].0, rotation[2].0, rotation[3].0])
            }
            None => Quat::IDENTITY,
        }
    }

    /// An arbitrary collection of program-specific strings.
    pub fn tags(&self) -> &[String] {
        self.tags.as_deref().unwrap_or_default()
    }

    /// Translation of a geometry.
    pub fn translation(&self) -> Vec3 {
        self.translation
            .map(|translation| vec3(translation[0].0, translation[1].0, translation[2].0))
            .unwrap_or(Vec3::ZERO)
    }
}

/// Holds a description of scene entities and tagged data.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct SceneAsset {
    #[serde(rename = "geometry")]
    geometries: Option<Box<[Geometry]>>,

    #[serde(rename = "ref")]
    references: Option<Box<[Reference]>>,
}

impl SceneAsset {
    /// Reads and processes scene source files into an existing `.pak` file buffer.
    pub fn bake(
        &self,
        rt: &Runtime,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        path: impl AsRef<Path>,
    ) -> anyhow::Result<SceneId> {
        // Early-out if we have already baked this scene
        let asset = self.clone().into();
        if let Some(h) = writer.lock().ctx.get(&asset) {
            return Ok(h.as_scene().unwrap());
        }

        let key = file_key(&project_dir, &path);

        info!("Baking scene: {}", key);

        let src_dir = parent(&path);

        let geometries = self
            .geometries()
            .iter()
            .map(|geometry| {
                let data = geometry
                    .data()
                    .map(|(key, value)| (key.clone(), value.clone().into()))
                    .collect();

                // all tags must be lower case (no localized text!)
                let mut tags = vec![];
                for tag in geometry.tags() {
                    let baked = tag.as_str().trim().to_lowercase();
                    if let Err(idx) = tags.binary_search(&baked) {
                        tags.insert(idx, baked);
                    }
                }

                let mut vertices = Vec::with_capacity(geometry.vertices.len() * size_of::<f32>());
                geometry
                    .vertices
                    .iter()
                    .map(|vertex| vertex.0.to_ne_bytes())
                    .for_each(|vertex| vertices.extend_from_slice(&vertex));

                GeometryData {
                    data,
                    id: geometry.id().map(|id| id.to_owned()),
                    indices: geometry.indices.to_vec(),
                    vertices,
                    rotation: geometry.rotation().into(),
                    tags,
                    translation: geometry.translation().into(),
                }
            })
            .collect::<Box<_>>();

        let references = self
            .refs()
            .iter()
            .map(|reference| {
                // all tags must be lower case (no localized text!)
                let mut tags = vec![];
                for tag in reference.tags() {
                    let baked = tag.as_str().trim().to_lowercase();
                    if let Err(idx) = tags.binary_search(&baked) {
                        tags.insert(idx, baked);
                    }
                }

                let data = reference
                    .data()
                    .map(|(key, value)| (key.clone(), value.clone().into()))
                    .collect();

                let materials = reference
                    .materials()
                    .iter()
                    .map(|material| match material {
                        AssetRef::Asset(material) => {
                            // Material asset specified inline
                            let material = material.clone();
                            (None, material)
                        }
                        AssetRef::Path(src) => {
                            if is_toml(src) {
                                // Asset file reference
                                let mut material = Asset::read(src)
                                    .context("Reading material asset")
                                    .expect("Unable to read material asset")
                                    .into_material()
                                    .expect("Not a material");
                                let src_dir = parent(src);
                                material.canonicalize(&project_dir, &src_dir);
                                (Some(src), material)
                            } else {
                                // Material color file reference
                                (None, MaterialAsset::new(src))
                            }
                        }
                    })
                    .map(|(src, mut material)| {
                        material
                            .bake(rt, writer, &project_dir, &src_dir, src)
                            .expect("material")
                    })
                    .collect();

                let mesh = reference
                    .mesh()
                    .map(|mesh| match mesh {
                        AssetRef::Asset(mesh) => {
                            // Mesh asset specified inline
                            let mesh = mesh.clone();
                            (None, mesh)
                        }
                        AssetRef::Path(src) => {
                            if is_toml(src) {
                                // Asset file reference
                                let mut mesh = Asset::read(src)
                                    .context("Reading mesh asset")
                                    .expect("Unable to read mesh asset")
                                    .into_mesh()
                                    .expect("Not a mesh");
                                let src_dir = parent(src);
                                mesh.canonicalize(&project_dir, &src_dir);
                                (Some(src), mesh)
                            } else {
                                // Mesh file reference
                                (None, MeshAsset::new(src))
                            }
                        }
                    })
                    .map(|(src, mesh)| mesh.bake(writer, &project_dir, src).expect("bake mesh"));

                ReferenceData {
                    data,
                    id: reference.id().map(str::to_owned),
                    materials,
                    mesh,
                    rotation: reference.rotation().into(),
                    tags,
                    translation: reference.translation().into(),
                }
            })
            .collect::<Box<_>>();

        let scene = Scene::new(geometries, references);

        let mut writer = writer.lock();
        if let Some(h) = writer.ctx.get(&asset) {
            return Ok(h.as_scene().unwrap());
        }

        let id = writer.push_scene(scene, key);
        writer.ctx.insert(asset, id.into());

        Ok(id)
    }

    /// Individual geometries within a scene.
    #[allow(unused)]
    pub fn geometries(&self) -> &[Geometry] {
        self.geometries.as_deref().unwrap_or_default()
    }

    /// Individual references within a scene.
    #[allow(unused)]
    pub fn refs(&self) -> &[Reference] {
        self.references.as_deref().unwrap_or_default()
    }
}

impl Canonicalize for SceneAsset {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        self.references
            .as_deref_mut()
            .unwrap_or_default()
            .iter_mut()
            .for_each(|reference| reference.canonicalize(&project_dir, &src_dir));
    }
}

/// Holds a description of one scene reference.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Reference {
    id: Option<String>,

    // Values
    euler: Option<Euler>,
    materials: Option<Vec<AssetRef<MaterialAsset>>>,
    #[serde(default, deserialize_with = "AssetRef::<MeshAsset>::de")]
    mesh: Option<AssetRef<MeshAsset>>,
    rotation: Option<Rotation>,
    translation: Option<[OrderedFloat<f32>; 3]>,

    // Tables must follow values
    data: Option<BTreeMap<String, Data>>,
    tags: Option<Vec<String>>,
}

impl Reference {
    /// An arbitrary collection of program-specific strings.
    #[allow(unused)]
    pub fn data(&self) -> impl Iterator<Item = (&String, &Data)> {
        self.data
            .as_ref()
            .map(|data| data.iter())
            .unwrap_or_default()
    }

    /// Euler ordering of the mesh orientation.
    pub fn euler(&self) -> EulerRot {
        match self.euler.unwrap_or(Euler::XYZ) {
            Euler::XYZ => EulerRot::XYZ,
            Euler::XZY => EulerRot::XZY,
            Euler::YXZ => EulerRot::YXZ,
            Euler::YZX => EulerRot::YZX,
            Euler::ZXY => EulerRot::ZXY,
            Euler::ZYX => EulerRot::ZYX,
        }
    }

    /// Main identifier of a reference, not required to be unique.
    #[allow(unused)]
    pub fn id(&self) -> Option<&str> {
        self.id.as_deref()
    }

    /// Optional direct reference to a mesh asset file.
    ///
    /// If specified, the mesh asset does not need to be referenced in any content file. If the
    /// mesh is referenced in a content file it will not be duplicated or cause any problems.
    ///
    /// May either be a `Mesh` asset specified inline or a mesh source file. Mesh source files
    /// may be either `.toml` `Mesh` asset files or direct references to `.glb`/`.gltf` files.
    pub fn mesh(&self) -> Option<&AssetRef<MeshAsset>> {
        self.mesh.as_ref()
    }

    /// Optional direct reference to a material asset files.
    ///
    /// If specified, the material assets do not need to be referenced in any content file. If the
    /// material is referenced in a content file it will not be duplicated or cause any problems.
    pub fn materials(&self) -> &[AssetRef<MaterialAsset>] {
        self.materials.as_deref().unwrap_or_default()
    }

    /// Any 3D orientation or orientation-like data.
    #[allow(unused)]
    pub fn rotation(&self) -> Quat {
        match self.rotation {
            Some(Rotation::Euler(rotation)) => Quat::from_euler(
                self.euler(),
                rotation[0].0.to_radians(),
                rotation[1].0.to_radians(),
                rotation[2].0.to_radians(),
            ),
            Some(Rotation::Quaternion(rotation)) => {
                Quat::from_array([rotation[0].0, rotation[1].0, rotation[2].0, rotation[3].0])
            }
            None => Quat::IDENTITY,
        }
    }

    /// An arbitrary collection of program-specific strings.
    #[allow(unused)]
    pub fn tags(&self) -> &[String] {
        self.tags.as_deref().unwrap_or_default()
    }

    /// Any 3D position or position-like data.
    #[allow(unused)]
    pub fn translation(&self) -> Vec3 {
        self.translation
            .map(|translation| vec3(translation[0].0, translation[1].0, translation[2].0))
            .unwrap_or(Vec3::ZERO)
    }
}

impl Canonicalize for Reference {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        if let Some(materials) = self.materials.as_mut() {
            for material in materials {
                material.canonicalize(&project_dir, &src_dir);
            }
        }

        if let Some(mesh) = self.mesh.as_mut() {
            mesh.canonicalize(&project_dir, &src_dir);
        }
    }
}

/// Encapsulates any scene data.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum Data {
    Array(Vec<Data>),
    Bool(bool),
    Float(OrderedFloat<f32>),
    Number(i32),
    String(String),
}

impl<'de> Data {
    fn de<D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct DataVisitor;

        impl<'de> Visitor<'de> for DataVisitor {
            type Value = Option<Data>;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("bool, number, string, or array of any")
            }

            fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(Some(Data::Bool(v)))
            }

            fn visit_char<E>(self, v: char) -> Result<Self::Value, E>
            where
                E: Error,
            {
                self.visit_string(v.to_string())
            }

            fn visit_f32<E>(self, v: f32) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_f64(v as _)
            }

            fn visit_f64<E>(self, v: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(Some(Data::Float(OrderedFloat(v as _))))
            }

            fn visit_i8<E>(self, v: i8) -> Result<Self::Value, E>
            where
                E: Error,
            {
                self.visit_i64(v as _)
            }

            fn visit_i16<E>(self, v: i16) -> Result<Self::Value, E>
            where
                E: Error,
            {
                self.visit_i64(v as _)
            }

            fn visit_i32<E>(self, v: i32) -> Result<Self::Value, E>
            where
                E: Error,
            {
                self.visit_i64(v as _)
            }

            fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if v >= i32::MIN as i64 && v <= i32::MAX as i64 {
                    Ok(Some(Data::Number(v as _)))
                } else {
                    Err(Error::invalid_type(
                        serde::de::Unexpected::Signed(v),
                        &"an i32",
                    ))
                }
            }

            fn visit_seq<A>(self, mut seq: A) -> Result<Self::Value, A::Error>
            where
                A: serde::de::SeqAccess<'de>,
            {
                let mut res = vec![];

                while let Some(item) = seq.next_element()? {
                    res.push(item);
                }

                Ok(Some(Data::Array(res)))
            }

            fn visit_str<E>(self, str: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_string(str.to_string())
            }

            fn visit_string<E>(self, v: String) -> Result<Self::Value, E>
            where
                E: Error,
            {
                Ok(Some(Data::String(v)))
            }

            fn visit_u8<E>(self, v: u8) -> Result<Self::Value, E>
            where
                E: Error,
            {
                self.visit_u64(v as _)
            }

            fn visit_u16<E>(self, v: u16) -> Result<Self::Value, E>
            where
                E: Error,
            {
                self.visit_u64(v as _)
            }

            fn visit_u32<E>(self, v: u32) -> Result<Self::Value, E>
            where
                E: Error,
            {
                self.visit_u64(v as _)
            }

            fn visit_u64<E>(self, v: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if v <= i32::MAX as u64 {
                    Ok(Some(Data::Number(v as _)))
                } else {
                    Err(Error::invalid_type(
                        serde::de::Unexpected::Unsigned(v),
                        &"an i32",
                    ))
                }
            }
        }

        deserializer.deserialize_any(DataVisitor)
    }
}

impl<'de> Deserialize<'de> for Data {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Data::de(deserializer).transpose().unwrap()
    }
}

impl From<Data> for DataData {
    fn from(value: Data) -> Self {
        match value {
            Data::Array(values) => DataData::Array(values.into_iter().map(Into::into).collect()),
            Data::Bool(value) => DataData::Bool(value),
            Data::Float(OrderedFloat(value)) => DataData::Float(value),
            Data::Number(value) => DataData::Number(value),
            Data::String(value) => DataData::String(value),
        }
    }
}
