//! Contains functions and types used to bake assets into .pak files
//!
//! Assets are regular art such as `.glb`, `.jpeg` and `.ttf` files.

mod anim;
mod asset;
mod bitmap;
mod blob;
mod content;
mod material;
mod mesh;
mod scene;
mod writer;

use {
    self::{
        asset::Asset,
        bitmap::BitmapAsset,
        blob::BlobAsset,
        material::{ColorRef, EmissiveRef, MaterialAsset, NormalRef, ScalarRef},
        mesh::MeshAsset,
        scene::AssetRef,
        writer::Writer,
    },
    crate::PakBuf,
    anyhow::Context,
    glob::glob,
    log::info,
    ordered_float::OrderedFloat,
    parking_lot::Mutex,
    serde::{
        Deserialize, Deserializer,
        de::{Error, SeqAccess, Visitor, value::SeqAccessDeserializer},
    },
    std::{
        collections::{BTreeSet, HashSet},
        env::var,
        fmt::{Debug, Formatter},
        fs::create_dir_all,
        num::FpCategory,
        path::{Path, PathBuf},
        sync::{
            Arc,
            atomic::{AtomicBool, Ordering},
        },
    },
    tokio::runtime::Runtime,
};

/// Given some parent directory and a filename, returns just the portion after the directory.
fn file_key(dir: impl AsRef<Path>, path: impl AsRef<Path>) -> String {
    let res_dir = dir.as_ref();
    let path = path.as_ref();

    let mut key = vec![];
    for part in path.ancestors() {
        if part == res_dir {
            break;
        }

        if !key.is_empty() {
            key.push("/".to_string());
        }

        if let Some(file_name) = part.file_name() {
            key.push(file_name.to_string_lossy().to_string());
        }
    }

    let key = key.into_iter().rev().collect::<String>();

    // Strip off the toml extension as needed
    let mut key = PathBuf::from(key);
    if is_toml(&key)
        && let Some(stem) = key.file_stem().map(ToOwned::to_owned)
    {
        key.set_file_name(stem);
    }

    key.to_str().unwrap_or_default().to_owned()
}

fn is_cargo_build() -> bool {
    var("CARGO").is_ok()
}

/// Returns `true` when a given path has the `.toml` file extension.
fn is_toml(path: impl AsRef<Path>) -> bool {
    path.as_ref()
        .extension()
        .and_then(|ext| ext.to_str())
        .filter(|ext| *ext == "toml")
        .is_some()
}

/// Returns either the parent directory of the given path or the project root if the path has no
/// parent.
fn parent(path: impl AsRef<Path>) -> PathBuf {
    path.as_ref()
        .parent()
        .map(|path| path.to_owned())
        .unwrap_or_else(|| PathBuf::from("/"))
}

fn project_path(dir: impl AsRef<Path>, path: impl AsRef<Path>) -> PathBuf {
    let path = path.as_ref();
    if !path.is_absolute() {
        return dir.as_ref().join(path);
    }

    path.components()
        .fold(dir.as_ref().to_path_buf(), |res, component| {
            if let std::path::Component::Normal(part) = component {
                res.join(part)
            } else {
                res
            }
        })
}

fn parse_hex_color(val: &str) -> Option<[u8; 4]> {
    let mut res = [u8::MAX; 4];
    let len = val.len();
    match len {
        4 | 5 => {
            res[0] = u8::from_str_radix(&val[1..2].repeat(2), 16).ok()?;
            res[1] = u8::from_str_radix(&val[2..3].repeat(2), 16).ok()?;
            res[2] = u8::from_str_radix(&val[3..4].repeat(2), 16).ok()?;
        }
        7 | 9 => {
            res[0] = u8::from_str_radix(&val[1..3], 16).ok()?;
            res[1] = u8::from_str_radix(&val[3..5], 16).ok()?;
            res[2] = u8::from_str_radix(&val[5..7], 16).ok()?;
        }
        _ => return None,
    }

    res[3] = match len {
        5 => u8::from_str_radix(&val[4..5].repeat(2), 16).ok()?,
        9 => u8::from_str_radix(&val[7..9], 16).ok()?,
        _ => u8::MAX,
    };

    Some(res)
}

fn parse_hex_scalar(val: &str) -> Option<u8> {
    match val.len() {
        2 => Some(u8::from_str_radix(&val[1..2].repeat(2), 16).ok()?),
        3 => Some(u8::from_str_radix(&val[1..3], 16).ok()?),
        _ => None,
    }
}

static CARGO_WATCHES_ENABLED: AtomicBool = AtomicBool::new(true);

fn re_run_if_changed(p: impl AsRef<Path>) {
    if is_cargo_build() && CARGO_WATCHES_ENABLED.load(Ordering::Relaxed) {
        println!("cargo:rerun-if-changed={}", p.as_ref().display());
    }
}

struct CargoWatchesGuard {
    enabled: bool,
}

impl CargoWatchesGuard {
    fn set(enabled: bool) -> Self {
        Self {
            enabled: CARGO_WATCHES_ENABLED.swap(enabled, Ordering::Relaxed),
        }
    }
}

impl Drop for CargoWatchesGuard {
    fn drop(&mut self) {
        CARGO_WATCHES_ENABLED.store(self.enabled, Ordering::Relaxed);
    }
}

/// Euler rotation sequences.
///
/// The angles are applied starting from the right. E.g. XYZ will first apply the z-axis rotation.
///
/// YXZ can be used for yaw (y-axis), pitch (x-axis), roll (z-axis).
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
pub enum Euler {
    /// Intrinsic three-axis rotation XYZ
    #[serde(rename = "xyz")]
    XYZ,
    /// Intrinsic three-axis rotation XZY
    #[serde(rename = "xzy")]
    XZY,
    /// Intrinsic three-axis rotation YXZ
    #[serde(rename = "yxz")]
    YXZ,
    /// Intrinsic three-axis rotation YZX
    #[serde(rename = "yzx")]
    YZX,
    /// Intrinsic three-axis rotation ZXY
    #[serde(rename = "zxy")]
    ZXY,
    /// Intrinsic three-axis rotation ZYX
    #[serde(rename = "zyx")]
    ZYX,
}

trait Canonicalize {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>);

    /// Gets the fully rooted source path.
    ///
    /// If `src` is relative, then `src_dir` is used to determine the relative parent.
    /// If `src` is absolute, then `project_dir` is considered to be its root.
    fn canonicalize_project_path(
        project_dir: impl AsRef<Path>,
        src_dir: impl AsRef<Path>,
        src: impl AsRef<Path>,
    ) -> PathBuf {
        // info!(
        //     "Getting path for {} in {} (res_dir={}, absolute={})",
        //     src.as_ref().display(),
        //     src_dir.as_ref().display(),
        //     project_dir.as_ref().display(),
        //     src.as_ref().is_absolute()
        // );

        // Absolute paths are 'project aka resource directory' absolute, not *your host file system*
        // absolute!
        let res = if src.as_ref().is_absolute() {
            // TODO: This could be way simpler!

            // Build an array of path items (file and directories) until the root
            let mut temp = Some(src.as_ref());
            let mut parts = vec![];
            while let Some(path) = temp {
                if let Some(part) = path.file_name() {
                    parts.push(part);
                    temp = path.parent();
                } else {
                    break;
                }
            }

            // Paste the incoming path (minus root) onto the res_dir parameter
            let mut temp = project_dir.as_ref().to_path_buf();
            for part in parts.iter().rev() {
                temp = temp.join(part);
            }

            temp
        } else {
            src_dir.as_ref().join(&src)
        };

        dunce::canonicalize(&res).unwrap_or(res)
    }
}

impl PakBuf {
    /// Returns the list of source files used to bake this pak, including all assets
    /// specified inline or within scenes.
    ///
    /// Includes the provided `src` parameter.
    pub fn source_files(src: impl AsRef<Path>) -> anyhow::Result<Box<[PathBuf]>> {
        Self::source_files_with_dir(&src, parent(&src))
    }

    /// Returns the list of source files used to bake this pak, including all assets
    /// specified inline or within scenes.
    ///
    /// Includes the provided `src` parameter. Asset globs and project-rooted paths are resolved
    /// from `dir` instead of the content file's parent directory.
    pub fn source_files_with_dir(
        src: impl AsRef<Path>,
        dir: impl AsRef<Path>,
    ) -> anyhow::Result<Box<[PathBuf]>> {
        fn handle_bitmap(res: &mut BTreeSet<PathBuf>, bitmap: &BitmapAsset) {
            if let Some(src) = bitmap.src() {
                res.insert(src.to_path_buf());
            }
        }

        fn handle_material(res: &mut BTreeSet<PathBuf>, material: &MaterialAsset) {
            match &material.color {
                Some(ColorRef::Asset(bitmap)) => handle_bitmap(res, bitmap),
                Some(ColorRef::Path(path)) => {
                    res.insert(path.to_path_buf());
                }
                _ => (),
            }

            if let Some(height) = &material.height {
                handle_scalar_ref(res, height);
            }

            match &material.emissive {
                Some(EmissiveRef::Asset(bitmap)) => handle_bitmap(res, bitmap),
                Some(EmissiveRef::Path(path)) => {
                    res.insert(path.to_path_buf());
                }
                _ => (),
            }

            if let Some(metal) = &material.metal {
                handle_scalar_ref(res, metal);
            }

            match &material.normal {
                Some(NormalRef::Asset(bitmap)) => handle_bitmap(res, bitmap),
                Some(NormalRef::Path(path)) => {
                    res.insert(path.to_path_buf());
                }
                None => (),
            }

            if let Some(rough) = &material.rough {
                handle_scalar_ref(res, rough);
            }

            if let Some(transmission) = &material.transmission {
                handle_scalar_ref(res, transmission);
            }
        }

        fn handle_mesh(res: &mut BTreeSet<PathBuf>, mesh: &MeshAsset) {
            if let Some(data) = mesh.data() {
                res.insert(data.to_path_buf());
            }

            if let Some(src) = mesh.src() {
                res.insert(src.to_path_buf());
            }
        }

        fn handle_scalar_ref(res: &mut BTreeSet<PathBuf>, scalar_ref: &ScalarRef) {
            match scalar_ref {
                ScalarRef::Asset(bitmap) => handle_bitmap(res, bitmap),
                ScalarRef::Path(path) => {
                    res.insert(path.to_path_buf());
                }
                _ => (),
            }
        }

        // Load the source file into an Asset::Content instance
        let src_dir = dir.as_ref().to_path_buf();
        let content = Asset::read(&src)?
            .into_content()
            .context("Unable to read asset file")?;

        let mut res = BTreeSet::new();

        res.insert(src.as_ref().to_path_buf());

        let enabled_groups = || content.groups().filter(|group| group.enabled());

        let mut excluded_assets = HashSet::new();
        for pattern in enabled_groups().flat_map(|group| group.exclude_globs()) {
            for path in glob(project_path(&src_dir, pattern).to_string_lossy().as_ref())? {
                let path = path?;

                excluded_assets.insert(path);
            }
        }

        for asset_glob in enabled_groups().flat_map(|group| group.asset_globs()) {
            let asset_paths = glob(
                project_path(&src_dir, asset_glob)
                    .to_string_lossy()
                    .as_ref(),
            )
            .context("Unable to glob source directory")?;
            for asset_path in asset_paths {
                let asset_path = asset_path?;
                if excluded_assets.contains(&asset_path) {
                    continue;
                }

                if asset_path
                    .extension()
                    .map(|ext| ext.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .to_lowercase()
                    .as_str()
                    == "toml"
                {
                    let asset = Asset::read(&asset_path)?;
                    let asset_parent = parent(&asset_path);

                    match asset {
                        Asset::Animation(mut anim) => {
                            anim.canonicalize(&src_dir, &asset_parent);

                            if let Some(src) = anim.src() {
                                res.insert(src.to_path_buf());
                            }
                        }
                        Asset::Bitmap(mut bitmap) => {
                            bitmap.canonicalize(&src_dir, &asset_parent);
                            handle_bitmap(&mut res, &bitmap);
                        }
                        Asset::BitmapFont(mut blob) | Asset::Blob(mut blob) => {
                            blob.canonicalize(&src_dir, &asset_parent);

                            if let Some(src) = blob.src() {
                                res.insert(src.to_path_buf());
                            }
                        }
                        Asset::Material(mut material) => {
                            material.canonicalize(&src_dir, &asset_parent);
                            handle_material(&mut res, &material);
                        }
                        Asset::Mesh(mut mesh) => {
                            mesh.canonicalize(&src_dir, &asset_parent);
                            handle_mesh(&mut res, &mesh);
                        }
                        Asset::Scene(mut scene) => {
                            scene.canonicalize(&src_dir, &asset_parent);

                            for scene_ref in scene.refs() {
                                if let Some(mesh) = scene_ref.mesh() {
                                    match mesh {
                                        AssetRef::Asset(mesh) => {
                                            handle_mesh(&mut res, mesh);
                                        }
                                        AssetRef::Path(path) => {
                                            if res.insert(path.to_path_buf()) && is_toml(path) {
                                                let Some(mut mesh) = Asset::read(path)?.into_mesh()
                                                else {
                                                    continue;
                                                };

                                                mesh.canonicalize(&src_dir, parent(path));
                                                handle_mesh(&mut res, &mesh);
                                            }
                                        }
                                    }
                                }

                                for material in scene_ref.materials() {
                                    match material {
                                        AssetRef::Asset(material) => {
                                            handle_material(&mut res, material);
                                        }
                                        AssetRef::Path(path) => {
                                            if res.insert(path.to_path_buf()) && is_toml(path) {
                                                let Some(mut material) =
                                                    Asset::read(path)?.into_material()
                                                else {
                                                    continue;
                                                };

                                                material.canonicalize(&src_dir, parent(path));
                                                handle_material(&mut res, &material);
                                            }
                                        }
                                    }
                                }
                            }
                        }
                        _ => (),
                    }
                }

                res.insert(asset_path);
            }
        }

        Ok(res.into_iter().collect())
    }

    pub fn bake(src: impl AsRef<Path>, dst: impl AsRef<Path>) -> anyhow::Result<()> {
        Self::bake_with_dir(&src, dst, parent(&src))
    }

    /// Bakes content into a `.pak` file using `dir` as the asset root.
    ///
    /// The content file `src` may live outside the asset root. Asset globs, project-rooted paths,
    /// and generated pak keys are resolved from `dir`.
    pub fn bake_with_dir(
        src: impl AsRef<Path>,
        dst: impl AsRef<Path>,
        dir: impl AsRef<Path>,
    ) -> anyhow::Result<()> {
        Self::bake_with_dir_impl(src, dst, dir, true)
    }

    /// Bakes content into a `.pak` file using `dir` as the asset root without emitting Cargo
    /// change watches.
    ///
    /// This is intended for build scripts that collect and emit their own precise watch list.
    pub fn bake_with_dir_without_cargo_watches(
        src: impl AsRef<Path>,
        dst: impl AsRef<Path>,
        dir: impl AsRef<Path>,
    ) -> anyhow::Result<()> {
        Self::bake_with_dir_impl(src, dst, dir, false)
    }

    fn bake_with_dir_impl(
        src: impl AsRef<Path>,
        dst: impl AsRef<Path>,
        dir: impl AsRef<Path>,
        cargo_watches: bool,
    ) -> anyhow::Result<()> {
        let _cargo_watches = CargoWatchesGuard::set(cargo_watches);

        re_run_if_changed(&src);

        let rt = Arc::new(Runtime::new()?);
        let mut tasks: Vec<tokio::task::JoinHandle<anyhow::Result<()>>> = vec![];
        let writer: Arc<Mutex<Writer>> = Arc::new(Mutex::new(Default::default()));

        // Load the source file into an Asset::Content instance
        let src_dir = dir.as_ref().to_path_buf();
        let content = Asset::read(&src)?
            .into_content()
            .context("Unable to read asset file")?;

        if let Some(compression) = content.compression() {
            writer.lock().with_compression_is(Some(compression));
        }

        let enabled_groups = || content.groups().filter(|group| group.enabled());

        let mut excluded_assets = HashSet::new();
        for pattern in enabled_groups().flat_map(|group| group.exclude_globs()) {
            for path in glob(project_path(&src_dir, pattern).to_string_lossy().as_ref())? {
                let path = path?;

                excluded_assets.insert(path);
            }
        }

        // Process each file we find as a separate runtime task
        for asset_glob in enabled_groups().flat_map(|group| group.asset_globs()) {
            let asset_paths = glob(
                project_path(&src_dir, asset_glob)
                    .to_string_lossy()
                    .as_ref(),
            )
            .context("Unable to glob source directory")?;
            for asset_path in asset_paths {
                let asset_path = asset_path.context("Unable to get asset path")?;
                if excluded_assets.contains(&asset_path) {
                    continue;
                }

                info!("processing {}", asset_path.display());

                re_run_if_changed(&asset_path);

                match asset_path
                    .extension()
                    .map(|ext| ext.to_string_lossy().into_owned())
                    .unwrap_or_default()
                    .to_lowercase()
                    .as_str()
                {
                    "glb" | "gltf" => {
                        // Note that direct references like this build a mesh, not an animation
                        // To build an animation you must specify a .toml file
                        let writer = Arc::clone(&writer);
                        let src_dir = src_dir.clone();
                        let asset_path = asset_path.clone();
                        tasks.push(rt.spawn_blocking(move || {
                            MeshAsset::new(&asset_path)
                                .bake(&writer, &src_dir, Some(&asset_path))
                                .context(asset_path.as_os_str().to_string_lossy().into_owned())?;
                            Ok(())
                        }));
                    }
                    "jpg" | "jpeg" | "png" | "bmp" | "tga" | "dds" | "webp" | "gif" | "ico"
                    | "tiff" => {
                        let writer = Arc::clone(&writer);
                        let src_dir = src_dir.clone();
                        let asset_path = asset_path.clone();
                        tasks.push(rt.spawn_blocking(move || {
                            BitmapAsset::new(&asset_path)
                                .bake_from_path(&writer, src_dir, Some(&asset_path))
                                .context(asset_path.as_os_str().to_string_lossy().into_owned())?;
                            Ok(())
                        }));
                    }
                    "toml" => {
                        let asset = Asset::read(&asset_path)?;
                        let asset_parent = parent(&asset_path);

                        match asset {
                            Asset::Animation(mut anim) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    anim.canonicalize(&src_dir, &asset_parent);
                                    anim.bake(&writer, src_dir, &asset_path).context(
                                        asset_path.as_os_str().to_string_lossy().into_owned(),
                                    )?;
                                    Ok(())
                                }));
                            }
                            Asset::Bitmap(mut bitmap) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    bitmap.canonicalize(&src_dir, &asset_parent);
                                    bitmap
                                        .bake_from_path(&writer, src_dir, Some(&asset_path))
                                        .context(
                                            asset_path.as_os_str().to_string_lossy().into_owned(),
                                        )?;
                                    Ok(())
                                }));
                            }
                            Asset::BitmapFont(mut blob) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    blob.canonicalize(&src_dir, &asset_parent);
                                    blob.bake_bitmap_font(&writer, src_dir, &asset_path)
                                        .context(
                                            asset_path.as_os_str().to_string_lossy().into_owned(),
                                        )?;
                                    Ok(())
                                }));
                            }
                            Asset::Blob(mut blob) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    blob.canonicalize(&src_dir, &asset_parent);
                                    blob.bake_from_path(&writer, src_dir, &asset_path).context(
                                        asset_path.as_os_str().to_string_lossy().into_owned(),
                                    )?;
                                    Ok(())
                                }));
                            }
                            Asset::Material(mut material) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                let rt2 = rt.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    material.canonicalize(&src_dir, &asset_parent);
                                    material
                                        .bake(&rt2, &writer, src_dir, Some(&asset_path))
                                        .context(
                                            asset_path.as_os_str().to_string_lossy().into_owned(),
                                        )?;
                                    Ok(())
                                }));
                            }
                            Asset::Mesh(mut mesh) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    mesh.canonicalize(&src_dir, &asset_parent);
                                    mesh.bake(&writer, &src_dir, Some(&asset_path)).context(
                                        asset_path.as_os_str().to_string_lossy().into_owned(),
                                    )?;
                                    Ok(())
                                }));
                            }
                            Asset::Scene(mut scene) => {
                                let writer = Arc::clone(&writer);
                                let src_dir = src_dir.clone();
                                let asset_path = asset_path.clone();
                                let asset_parent = asset_parent.clone();
                                let rt2 = rt.clone();
                                tasks.push(rt.spawn_blocking(move || {
                                    scene.canonicalize(&src_dir, &asset_parent);
                                    scene.bake(&rt2, &writer, &src_dir, &asset_path).context(
                                        asset_path.as_os_str().to_string_lossy().into_owned(),
                                    )?;
                                    Ok(())
                                }));
                            }
                            _ => anyhow::bail!("unhandled asset type"),
                        }
                    }
                    _ => {
                        let writer = Arc::clone(&writer);
                        let src_dir = src_dir.clone();
                        let asset_path = asset_path.clone();
                        tasks.push(rt.spawn_blocking(move || {
                            let blob = BlobAsset::new(&asset_path);
                            blob.bake(&writer, &src_dir)
                                .context(asset_path.as_os_str().to_string_lossy().into_owned())?;
                            Ok(())
                        }));
                    }
                }
            }
        }

        let result: anyhow::Result<()> = rt.block_on(async {
            for task in tasks.into_iter() {
                task.await.context("spawned task failed")??;
            }

            let dst = dst.as_ref().to_path_buf();
            if let Some(parent) = dst.parent() {
                create_dir_all(parent).context("Unable to create directory")?;
            }

            writer
                .lock()
                .write(&dst)
                .context("Unable to write pak file")?;

            Ok(())
        });

        result
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum Rotation {
    /// A three component euler rotation.
    Euler([OrderedFloat<f32>; 3]),

    /// A four component quaternion rotation.
    Quaternion([OrderedFloat<f32>; 4]),
}

impl<'de> Rotation {
    /// Deserialize from any of absent or:
    ///
    /// euler xyz:
    /// .. = [1.0, 2.0, 3.0]
    ///
    /// quaternion xyzw:
    /// .. = [1.0, 2.0, 3.0, 0.0]
    fn de<D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct RotationVisitor;

        impl<'de> Visitor<'de> for RotationVisitor {
            type Value = Option<Rotation>;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("floating point sequence of length 3 or 4")
            }

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let val: Vec<f32> = Deserialize::deserialize(SeqAccessDeserializer::new(seq))?;

                if !matches!(val.len(), 3 | 4) {
                    return Err(Error::custom("expected 3 or 4 values"));
                }

                for val in &val {
                    match val.classify() {
                        FpCategory::Zero | FpCategory::Normal => (),
                        _ => return Err(Error::custom("expected a normal floating point value")),
                    }
                }

                Ok(Some(if val.len() == 3 {
                    Rotation::Euler([
                        OrderedFloat(val[0]),
                        OrderedFloat(val[1]),
                        OrderedFloat(val[2]),
                    ])
                } else {
                    Rotation::Quaternion([
                        OrderedFloat(val[0]),
                        OrderedFloat(val[1]),
                        OrderedFloat(val[2]),
                        OrderedFloat(val[3]),
                    ])
                }))
            }
        }

        deserializer.deserialize_seq(RotationVisitor)
    }
}

impl<'de> Deserialize<'de> for Rotation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        Rotation::de(deserializer)?.ok_or_else(|| D::Error::custom("expected rotation"))
    }
}
