use {
    super::{
        super::scene::{GeometryData, SceneRefData},
        file_key, is_toml,
        material::Material,
        model::Model,
        parent, Asset, Canonicalize, Euler, Rotation, SceneBuf, SceneId, Writer,
    },
    anyhow::Context,
    glam::{vec3, EulerRot, Quat, Vec3},
    log::info,
    ordered_float::OrderedFloat,
    parking_lot::Mutex,
    serde::{
        de::{
            value::{MapAccessDeserializer, SeqAccessDeserializer},
            MapAccess, SeqAccess, Visitor,
        },
        Deserialize, Deserializer,
    },
    std::{
        f32::consts::PI,
        fmt::Formatter,
        io::Error,
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
    euler: Option<Euler>,
    id: Option<String>,
    indices: Box<[u32]>,
    vertices: Box<[OrderedFloat<f32>]>,
    position: Option<[OrderedFloat<f32>; 3]>,
    rotation: Option<Rotation>,
    tags: Option<Box<[String]>>,
}

impl Geometry {
    /// Euler ordering of the model orientation.
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

    /// World location of a geometry.
    pub fn position(&self) -> Vec3 {
        self.position
            .map(|position| vec3(position[0].0, position[1].0, position[2].0))
            .unwrap_or(Vec3::ZERO)
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
}

/// Holds a description of scene entities and tagged data.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Scene {
    // (Values here)

    // Tables must follow values
    #[serde(rename = "geometry")]
    geometries: Option<Box<[Geometry]>>,

    #[serde(rename = "ref")]
    refs: Option<Box<[SceneRef]>>,
}

impl Scene {
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

        let mut geometries = Vec::with_capacity(
            self.geometries
                .as_ref()
                .map(|geometries| geometries.len())
                .unwrap_or_default(),
        );
        for geometry in self.geometries() {
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

            geometries.push(GeometryData {
                id: geometry.id().map(|id| id.to_owned()),
                indices: geometry.indices.to_vec(),
                vertices,
                position: geometry.position().into(),
                rotation: geometry.rotation().into(),
                tags,
            });
        }

        let mut refs = Vec::with_capacity(
            self.refs
                .as_ref()
                .map(|refs| refs.len())
                .unwrap_or_default(),
        );
        for scene_ref in self.refs() {
            // all tags must be lower case (no localized text!)
            let mut tags = vec![];
            for tag in scene_ref.tags() {
                let baked = tag.as_str().trim().to_lowercase();
                if let Err(idx) = tags.binary_search(&baked) {
                    tags.insert(idx, baked);
                }
            }

            let materials = scene_ref
                .materials()
                .iter()
                .map(|material| match material {
                    AssetRef::Asset(material) => {
                        // Material asset specified inline
                        let material = material.clone();
                        (None, material)
                    }
                    AssetRef::Path(src) => {
                        if is_toml(&src) {
                            // Asset file reference
                            let mut material = Asset::read(&src)
                                .context("Reading material asset")
                                .expect("Unable to read material asset")
                                .into_material()
                                .expect("Not a material");
                            let src_dir = parent(src);
                            material.canonicalize(&project_dir, &src_dir);
                            (Some(src), material)
                        } else {
                            // Material color file reference
                            (None, Material::new(src))
                        }
                    }
                })
                .map(|(src, mut material)| {
                    material
                        .bake(rt, writer, &project_dir, &src_dir, src)
                        .expect("material")
                })
                .collect();

            let model = scene_ref
                .model()
                .map(|model| match model {
                    AssetRef::Asset(model) => {
                        // Model asset specified inline
                        let model = model.clone();
                        (None, model)
                    }
                    AssetRef::Path(src) => {
                        if is_toml(&src) {
                            // Asset file reference
                            let mut model = Asset::read(&src)
                                .context("Reading model asset")
                                .expect("Unable to read model asset")
                                .into_model()
                                .expect("Not a model");
                            let src_dir = parent(src);
                            model.canonicalize(&project_dir, &src_dir);
                            (Some(src), model)
                        } else {
                            // Model file reference
                            (None, Model::new(src))
                        }
                    }
                })
                .map(|(src, model)| model.bake(writer, &project_dir, src).expect("bake model"));

            refs.push(SceneRefData {
                id: scene_ref.id().map(|id| id.to_owned()),
                materials,
                model,
                position: scene_ref.position().into(),
                rotation: scene_ref.rotation().into(),
                tags,
            });
        }

        let scene = SceneBuf::new(geometries.into_iter(), refs.into_iter());

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
    pub fn refs(&self) -> &[SceneRef] {
        self.refs.as_deref().unwrap_or_default()
    }
}

impl Canonicalize for Scene {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        self.refs
            .as_deref_mut()
            .unwrap_or_default()
            .iter_mut()
            .for_each(|scene_ref| scene_ref.canonicalize(&project_dir, &src_dir));
    }
}

/// Holds a description of one scene reference.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct SceneRef {
    euler: Option<Euler>,
    id: Option<String>,
    materials: Option<Vec<AssetRef<Material>>>,

    #[serde(default, deserialize_with = "AssetRef::<Model>::de")]
    model: Option<AssetRef<Model>>,

    position: Option<[OrderedFloat<f32>; 3]>,
    rotation: Option<Rotation>,

    // Tables must follow values
    tags: Option<Vec<String>>,
}

impl SceneRef {
    /// Euler ordering of the model orientation.
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

    /// Optional direct reference to a model asset file.
    ///
    /// If specified, the model asset does not need to be referenced in any content file. If the
    /// model is referenced in a content file it will not be duplicated or cause any problems.
    ///
    /// May either be a `Model` asset specified inline or a model source file. Model source files
    /// may be either `.toml` `Model` asset files or direct references to `.glb`/`.gltf` files.
    pub fn model(&self) -> Option<&AssetRef<Model>> {
        self.model.as_ref()
    }

    /// Optional direct reference to a material asset files.
    ///
    /// If specified, the material assets do not need to be referenced in any content file. If the
    /// material is referenced in a content file it will not be duplicated or cause any problems.
    pub fn materials(&self) -> &[AssetRef<Material>] {
        self.materials.as_deref().unwrap_or_default()
    }

    /// Any 3D position or position-like data.
    #[allow(unused)]
    pub fn position(&self) -> Vec3 {
        self.position
            .map(|position| vec3(position[0].0, position[1].0, position[2].0))
            .unwrap_or(Vec3::ZERO)
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
}

impl Canonicalize for SceneRef {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        if let Some(materials) = self.materials.as_mut() {
            for material in materials {
                material.canonicalize(&project_dir, &src_dir);
            }
        }

        if let Some(model) = self.model.as_mut() {
            model.canonicalize(&project_dir, &src_dir);
        }
    }
}
