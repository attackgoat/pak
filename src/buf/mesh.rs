use {
    super::{Canonicalize, Euler, Rotation, Writer, blob::BlobAsset, file_key, re_run_if_changed},
    crate::{
        MeshId,
        mesh::{Joint, Mesh, Primitive, Skin, VertexType},
    },
    anyhow::Context,
    glam::{EulerRot, Mat4, Quat, Vec3, vec3},
    gltf::{
        Buffer, Node,
        buffer::Data,
        import,
        mesh::{Mode, Reader, util::ReadIndices},
    },
    log::{info, trace, warn},
    meshopt::{
        SimplifyOptions, VertexDataAdapter, optimize_overdraw_in_place,
        optimize_vertex_cache_in_place, quantize_unorm, remap_index_buffer, simplify, unstripify,
    },
    ordered_float::OrderedFloat,
    parking_lot::Mutex,
    serde::{
        Deserialize, Deserializer,
        de::{SeqAccess, Visitor, value::SeqAccessDeserializer},
    },
    std::{
        collections::{BTreeSet, HashMap, HashSet},
        fmt::Formatter,
        io::{Error, ErrorKind},
        iter::repeat,
        num::FpCategory,
        path::{Path, PathBuf},
        sync::Arc,
        u16,
    },
};

fn extract_transform(node: &Node) -> Mat4 {
    let (translation, rotation, scale) = node.transform().decomposed();
    let translation = Vec3::from_array(translation);
    let rotation = Quat::from_array(rotation);
    let scale = Vec3::from_array(scale);

    Mat4::from_scale_rotation_translation(scale, rotation, translation)
}

/// Holds a description of `.glb` or `.gltf` 3D meshes.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct MeshAsset {
    data: Option<PathBuf>,
    euler: Option<Euler>,
    flip_x: Option<bool>,
    flip_y: Option<bool>,
    flip_z: Option<bool>,
    ignore_skin: Option<bool>,
    lod: Option<bool>,
    lod_lock_border: Option<bool>,
    lod_target_error: Option<OrderedFloat<f32>>,
    min_lod_triangles: Option<usize>,

    /// The artist-provided name of a mesh within the source file.
    name: Option<String>,

    normals: Option<bool>,
    offset: Option<[OrderedFloat<f32>; 3]>,
    optimize: Option<bool>,
    overdraw_threshold: Option<OrderedFloat<f32>>,

    rotation: Option<Rotation>,

    #[serde(default, deserialize_with = "Scale::de")]
    scale: Option<Scale>,

    scene_name: Option<String>,
    shadow: Option<bool>,
    src: PathBuf,
    tangents: Option<bool>,
}

impl MeshAsset {
    pub const DEFAULT_LOD_MIN: usize = 64;
    pub const DEFAULT_LOD_TARGET_ERROR: f32 = 0.05;

    pub fn new(src: impl AsRef<Path>) -> Self {
        Self {
            data: None,
            euler: None,
            flip_x: None,
            flip_y: None,
            flip_z: None,
            ignore_skin: None,
            lod: None,
            lod_lock_border: None,
            lod_target_error: None,
            min_lod_triangles: None,
            name: None,
            normals: None,
            offset: None,
            optimize: None,
            overdraw_threshold: None,
            rotation: None,
            scale: None,
            scene_name: None,
            shadow: None,
            src: src.as_ref().to_path_buf(),
            tangents: None,
        }
    }

    /// Reads and processes 3D mesh source files into an existing `.pak` file buffer.
    pub fn bake(
        &self,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        path: Option<impl AsRef<Path>>,
    ) -> anyhow::Result<MeshId> {
        // Early-out if we have already baked this mesh
        let asset = self.clone().into();

        if let Some(id) = writer.lock().ctx.get(&asset) {
            return Ok(id.as_mesh().unwrap());
        }

        self.re_run_if_changed();

        // If a path is given it will be available as a key inside the .pak (paths are not
        // given if the asset is specified inline - those are only available in the .pak via ID)
        let key = path.as_ref().map(|path| file_key(&project_dir, path));
        if let Some(key) = &key {
            // This mesh will be accessible using this key
            info!("Baking mesh: {}", key);
        } else {
            // This mesh will only be accessible using the handle
            info!(
                "Baking mesh: {} (inline)",
                file_key(&project_dir, self.src())
            );
        }

        let mut mesh = self
            .to_mesh()
            .map_err(|err| Error::new(ErrorKind::InvalidData, err))
            .context("Creating mesh buffer")?;

        // Bake the unstructured data blob too
        if let Some(data) = &self.data {
            let data_id = BlobAsset::new(data)
                .bake(writer, project_dir)
                .context("Baking unstructered mesh data")?;
            mesh.set_data(data_id);
        }

        // Check again to see if we are the first one to finish this
        let mut writer = writer.lock();
        if let Some(id) = writer.ctx.get(&asset) {
            return Ok(id.as_mesh().unwrap());
        }

        let id = writer.push_mesh(mesh, key);
        writer.ctx.insert(asset, id.into());

        Ok(id)
    }

    fn calculate_lods(
        &self,
        indices: &[u32],
        vertex_buf: &[u8],
        vertex_stride: usize,
    ) -> Vec<Vec<u32>> {
        let mut res = vec![Vec::from(indices)];

        if !self.lod() {
            return res;
        }

        let target_error = self.lod_target_error();
        let target_ratio = 1.0 + target_error;
        let min_triangles = self.min_lod_triangles();
        let vertices = VertexDataAdapter::new(vertex_buf, vertex_stride, 0).unwrap();
        let opts = if self.lod_lock_border() {
            SimplifyOptions::LockBorder
        } else {
            SimplifyOptions::None
        };

        loop {
            let target_count = (res.last().unwrap().len() / 3) >> 1;
            if target_count < min_triangles {
                break;
            }

            let lod = simplify(indices, &vertices, target_count, target_error, opts, None);
            let lod_count = lod.len() / 3;
            let lod_ratio = lod_count as f32 / target_count as f32;
            if lod_ratio > 1.0 || lod_ratio < target_ratio {
                break;
            }

            res.push(lod);
        }

        res
    }

    fn convert_triangle_fan_to_list(indices: &mut Vec<u32>) {
        if indices.is_empty() {
            return;
        }

        indices.reserve_exact((indices.len() - 1) >> 1);
        let mut idx = 3;
        while idx < indices.len() {
            indices.insert(idx, 0);
            idx += 3;
        }
    }

    fn convert_triangle_strip_to_list(indices: &mut Vec<u32>, restart_index: u32) {
        *indices = unstripify(indices, restart_index).expect("Unable to unstripify index buffer");
    }

    /// Optional associated unstructured data.
    pub fn data(&self) -> Option<&Path> {
        self.data.as_deref()
    }

    /// When `true` levels of detail will be generated for all meshes.
    pub fn lod(&self) -> bool {
        self.lod.unwrap_or_default()
    }

    /// When `true` levels of detail vertices that lie on the topological border of the mesh will be
    /// locked in place such that they don’t move during simplification.
    ///
    /// This can be valuable to simplify independent chunks of a mesh, for example terrain, to
    /// ensure that individual levels of detail can be stitched together later without gaps.
    pub fn lod_lock_border(&self) -> bool {
        self.lod_lock_border.unwrap_or_default()
    }

    /// The "fitting" value which levels of detail use to determine that further simplication will
    /// not greatly change a mesh.
    pub fn lod_target_error(&self) -> f32 {
        self.lod_target_error
            .unwrap_or(OrderedFloat(Self::DEFAULT_LOD_TARGET_ERROR))
            .0
    }

    /// The number of triangles below which further level of details are not calculated.
    ///
    /// Note: The last level of detail may have no less than half this number of triangles.
    pub fn min_lod_triangles(&self) -> usize {
        self.min_lod_triangles
            .unwrap_or(Self::DEFAULT_LOD_MIN)
            .clamp(1, usize::MAX)
    }

    /// When `true` (the default) normal values will be stored (or generated if needed).
    pub fn normals(&self) -> bool {
        self.normals.unwrap_or(true)
    }

    /// Translation of the mesh origin.
    pub fn offset(&self) -> Vec3 {
        self.offset
            .map(|offset| vec3(offset[0].0, offset[1].0, offset[2].0))
            .unwrap_or(Vec3::ZERO)
    }

    /// When `true` this mesh will be optmizied using the meshopt library.
    ///
    /// Optimization includes vertex cache, overdraw, and fetch support.
    pub fn optimize(&self) -> bool {
        self.optimize.unwrap_or(true)
    }

    /// At the very least this function will re-index the vertices, and optionally may
    /// perform full meshopt optimization.
    fn optimize_mesh(
        &self,
        indices: &mut Vec<u32>,
        vertex_buf: &mut Vec<u8>,
        vertex_stride: usize,
    ) {
        // TODO: PR these functions
        // HACK: Need to have a version of these functions which specify stride
        mod hack {
            pub fn generate_vertex_remap(
                indices: &mut [u32],
                vertex_buf: &mut [u8],
                vertex_stride: usize,
            ) -> (usize, Vec<u32>) {
                let vertex_count = vertex_buf.len() / vertex_stride;
                let mut remap: Vec<u32> = vec![0; vertex_count];
                let remap_count = unsafe {
                    meshopt::ffi::meshopt_generateVertexRemap(
                        remap.as_mut_ptr().cast(),
                        indices.as_ptr().cast(),
                        indices.len(),
                        vertex_buf.as_ptr().cast(),
                        vertex_count,
                        vertex_stride,
                    )
                };

                (remap_count, remap)
            }

            pub fn optimize_vertex_fetch_in_place(
                indices: &mut [u32],
                vertex_buf: &mut [u8],
                vertex_stride: usize,
            ) {
                let vertex_count = vertex_buf.len() / vertex_stride;

                let res = unsafe {
                    meshopt::ffi::meshopt_optimizeVertexFetch(
                        vertex_buf.as_mut_ptr().cast(),
                        indices.as_mut_ptr().cast(),
                        indices.len(),
                        vertex_buf.as_ptr().cast(),
                        vertex_count,
                        vertex_stride,
                    )
                };

                // This should be true because we expect remapped (..unique..) vertices
                assert_eq!(res, vertex_count);
            }

            pub fn remap_vertex_buffer(
                vertex_buf: &[u8],
                vertex_count: usize,
                vertex_stride: usize,
                remap: &[u32],
            ) -> Vec<u8> {
                let mut res = vec![0u8; vertex_count * vertex_stride];

                unsafe {
                    meshopt::ffi::meshopt_remapVertexBuffer(
                        res.as_mut_ptr().cast(),
                        vertex_buf.as_ptr().cast(),
                        vertex_buf.len() / vertex_stride,
                        vertex_stride,
                        remap.as_ptr().cast(),
                    );
                }

                res
            }
        }

        // Generate an index buffer from a naively indexed vertex buffer or reindex an existing one
        let (vertex_count, remap) = hack::generate_vertex_remap(indices, vertex_buf, vertex_stride);
        *indices = remap_index_buffer(Some(indices), vertex_buf.len() / vertex_stride, &remap);
        *vertex_buf = hack::remap_vertex_buffer(vertex_buf, vertex_count, vertex_stride, &remap);

        assert_eq!(indices.len() % 3, 0);
        assert_eq!(vertex_buf.len() % vertex_stride, 0);
        assert_eq!(vertex_buf.len() / vertex_stride, vertex_count);

        // Run the suggested routines from meshopt: https://github.com/gwihlidal/meshopt-rs#pipeline
        if self.optimize() {
            let vertices = VertexDataAdapter::new(vertex_buf, vertex_stride, 0).unwrap();

            // HACK: These functions take immutable borrows, BUT USES MUTABLE!
            // See: https://github.com/gwihlidal/meshopt-rs/pull/26 not yet released
            optimize_vertex_cache_in_place(indices, vertex_count);
            optimize_overdraw_in_place(indices, &vertices, self.overdraw_threshold());

            hack::optimize_vertex_fetch_in_place(indices, vertex_buf, vertex_stride);
        }
    }

    /// Determines how much the optimization algorithm can compromise the vertex cache hit ratio.
    ///
    /// A value of 1.05 means that the resulting ratio should be at most 5% worse than before the
    /// optimization.
    pub fn overdraw_threshold(&self) -> f32 {
        self.overdraw_threshold.unwrap_or(OrderedFloat(1.05)).0
    }

    fn read_skin(node: &Node, bufs: &[Data], transform: Mat4) -> Option<Skin> {
        node.skin().and_then(|skin| {
            let inverse_binds = skin
                .reader(|buf| bufs.get(buf.index()).map(|data| data.0.as_slice()))
                .read_inverse_bind_matrices()
                .map(|data| {
                    data.map(|matrix| {
                        let inverse_bind = Mat4::from_cols_array_2d(&matrix);
                        let bind = inverse_bind.inverse();

                        (transform * bind).inverse()
                    })
                    .collect::<Box<_>>()
                })
                .unwrap_or_default();

            if inverse_binds.is_empty() {
                warn!("Unable to read inverse bind matrices");

                return None;
            }

            if inverse_binds.len() != skin.joints().len() {
                warn!("Incompatible joints found");

                return None;
            }

            if skin.joints().any(|joint| joint.name().is_none()) {
                warn!("Unnamed joints found");

                return None;
            }

            {
                let mut joint_names = HashSet::new();
                for joint_name in skin.joints().map(|joint| joint.name().unwrap()) {
                    if !joint_names.insert(joint_name) {
                        warn!("Duplicate joint names found");

                        return None;
                    }
                }
            }

            let mut parents = HashMap::with_capacity(skin.joints().len());
            for (index, joint) in skin.joints().enumerate() {
                for child in joint.children() {
                    if parents.insert(child.index(), index).is_some() {
                        warn!("Invalid skeleton hierarchy found");

                        return None;
                    }
                }
            }

            let mut joints = Vec::with_capacity(skin.joints().len());
            for (idx, joint) in skin.joints().enumerate() {
                joints.push(Joint {
                    parent_index: parents.get(&joint.index()).copied().unwrap_or(idx),
                    inverse_bind: inverse_binds[idx].to_cols_array(),
                    name: joint.name().unwrap_or_default().to_string(),
                });
            }

            Some(Skin::new(joints))
        })
    }

    fn read_vertices<'a, 's, F>(data: Reader<'a, 's, F>) -> (u32, VertexData)
    where
        F: Clone + Fn(Buffer<'a>) -> Option<&'s [u8]>,
    {
        let positions = data
            .read_positions()
            .map(|positions| positions.collect::<Vec<_>>())
            .unwrap_or_default();

        let (restart_index, indices) = {
            let indices = data.read_indices().map(|indices| {
                (
                    match indices {
                        ReadIndices::U8(_) => u8::MAX as u32,
                        ReadIndices::U16(_) => u16::MAX as u32,
                        ReadIndices::U32(_) => u32::MAX,
                    },
                    indices.into_u32().collect::<Vec<_>>(),
                )
            });

            if indices.is_none() {
                warn!("Missing indices!");
            }

            indices.unwrap_or_else(|| (u32::MAX, (0..positions.len() as u32).collect()))
        };

        let textures = {
            let mut texture0 = data
                .read_tex_coords(0)
                .map(|data| data.into_f32())
                .map(|tex_coords| tex_coords.collect::<Vec<_>>())
                .unwrap_or_default();

            if !texture0.is_empty() {
                texture0.resize(positions.len(), Default::default());
            }

            let mut texture1 = data
                .read_tex_coords(1)
                .map(|data| data.into_f32())
                .map(|tex_coords| tex_coords.collect::<Vec<_>>())
                .unwrap_or_default();

            if !texture1.is_empty() {
                texture1.resize(positions.len(), Default::default());
            }

            (texture0, texture1)
        };

        let normals = {
            let mut normals = data
                .read_normals()
                .map(|normals| normals.collect::<Vec<_>>())
                .unwrap_or_default();

            if !normals.is_empty() {
                normals.resize(positions.len(), Default::default());
            }

            normals
        };

        let tangents = {
            let mut tangents = data
                .read_tangents()
                .map(|tangents| tangents.collect::<Vec<_>>())
                .unwrap_or_default();

            if !tangents.is_empty() {
                tangents.resize(positions.len(), Default::default());
            }

            tangents
        };

        let joints = data
            .read_joints(0)
            .map(|joints| {
                let mut res = joints
                    .into_u16()
                    .map(|joints| {
                        #[cfg(debug_assertions)]
                        for joint in joints {
                            assert!(joint <= u8::MAX as u16);
                        }

                        joints[0] as u32
                            | (joints[1] as u32) << 8
                            | (joints[2] as u32) << 16
                            | (joints[3] as u32) << 24
                    })
                    .collect::<Vec<_>>();
                res.resize(positions.len(), 0);
                res
            })
            .unwrap_or_default();
        let weights = data
            .read_weights(0)
            .map(|weights| {
                let mut res = weights
                    .into_f32()
                    .map(|weights| {
                        #[cfg(debug_assertions)]
                        for weight in weights {
                            assert!(weight >= 0.0);
                            assert!(weight <= 1.0);

                            let weight = quantize_unorm(weight, 8);

                            assert!(weight <= u8::MAX as i32);
                            assert!(weight >= u8::MIN as i32);
                        }

                        (quantize_unorm(weights[0], 8)
                            | (quantize_unorm(weights[1], 8) << 8)
                            | (quantize_unorm(weights[2], 8) << 16)
                            | (quantize_unorm(weights[3], 8) << 24)) as u32
                    })
                    .collect::<Vec<_>>();
                res.resize(positions.len(), 0);
                res
            })
            .unwrap_or_default();
        let has_skin = joints.len() == positions.len() && weights.len() == positions.len();
        let skin = if has_skin {
            Some((joints, weights))
        } else {
            None
        };

        (
            restart_index,
            VertexData {
                indices,
                normals,
                positions,
                skin,
                tangents,
                textures,
            },
        )
    }

    /// Orientation of the mesh.
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

    /// Scaling of the mesh.
    pub fn scale(&self) -> Vec3 {
        self.scale
            .map(|scale| match scale {
                Scale::Array(scale) => vec3(scale[0].0, scale[1].0, scale[2].0),
                Scale::Value(scale) => vec3(scale.0, scale.0, scale.0),
            })
            .unwrap_or(Vec3::ONE)
    }

    /// When `true` position-only shadow meshes will be generated.
    ///
    /// Note: Skinned meshes will contain position, joints, and weights.
    pub fn shadow(&self) -> bool {
        self.shadow.unwrap_or_default()
    }

    /// The mesh file source.
    pub fn src(&self) -> &Path {
        self.src.as_path()
    }

    /// When `true` (the default) tangent values will be stored (or generated if needed).
    pub fn tangents(&self) -> bool {
        self.tangents.unwrap_or(true)
    }

    fn to_mesh(&self) -> anyhow::Result<Mesh> {
        // Load the mesh nodes from this GLTF file
        let (doc, bufs, _) = import(self.src())
            .with_context(|| format!("Importing mesh {}", self.src().display()))?;
        let scene = self
            .scene_name
            .as_deref()
            .and_then(|name| doc.scenes().find(|scene| scene.name() == Some(&name)))
            .or_else(|| doc.default_scene())
            .or_else(|| doc.scenes().next())
            .expect("No scene found");
        let mut mesh_nodes = scene.nodes().filter(|node| node.mesh().is_some());
        let node = self
            .name
            .as_deref()
            .and_then(|name| mesh_nodes.find(|node| node.name() == Some(&name)))
            .or_else(|| mesh_nodes.next())
            .expect("No mesh found");
        let allow_skin = !self.ignore_skin.unwrap_or_default();
        let mesh_transform =
            Mat4::from_scale_rotation_translation(Vec3::ONE, self.rotation(), self.offset());

        info!("Loading mesh {}", node.name().unwrap_or_default());

        let skin = allow_skin
            .then(|| Self::read_skin(&node, &bufs, mesh_transform))
            .flatten();
        let transform = mesh_transform * extract_transform(&node);
        let parts = node
            .mesh()
            .unwrap()
            .primitives()
            .filter_map(|primitive| match primitive.mode() {
                Mode::TriangleFan | Mode::TriangleStrip | Mode::Triangles => {
                    trace!(
                        "Reading mesh \"{}\" (material index {})",
                        node.name().unwrap_or_default(),
                        if primitive.material().index().is_some() {
                            format!("{}", primitive.material().index().unwrap_or_default())
                        } else {
                            "unset".to_string()
                        }
                    );

                    // Read material and vertex data
                    let material = primitive.material().index().unwrap_or_default();
                    let (restart_index, mut vertices) = Self::read_vertices(
                        primitive.reader(|buf| bufs.get(buf.index()).map(|data| data.0.as_slice())),
                    );

                    // Convert unsupported modes (meshopt requires triangles)
                    match primitive.mode() {
                        Mode::TriangleFan => {
                            Self::convert_triangle_fan_to_list(&mut vertices.indices)
                        }
                        Mode::TriangleStrip => Self::convert_triangle_strip_to_list(
                            &mut vertices.indices,
                            restart_index,
                        ),
                        _ => (),
                    }

                    if self.flip_x.unwrap_or_default() {
                        for [x, _y, _z] in &mut vertices.positions {
                            *x *= -1.0;
                        }
                    }

                    if self.flip_y.unwrap_or_default() {
                        for [_x, y, _z] in &mut vertices.positions {
                            *y *= -1.0;
                        }
                    }

                    if self.flip_z.unwrap_or_default() {
                        for [_x, _y, z] in &mut vertices.positions {
                            *z *= -1.0;
                        }
                    }

                    vertices.transform(transform);

                    Some((material, vertices))
                }
                _ => None,
            })
            .collect::<Vec<_>>();

        // Figure out which unique materials are used on these target mesh primitives and convert
        // those to a map of "Mesh Local" material index from "Gltf File" material index
        // This makes the final materials used index as 0, 1, 2, etc
        let materials = parts
            .iter()
            .map(|(material, ..)| *material)
            .collect::<BTreeSet<_>>()
            .into_iter()
            .enumerate()
            .map(|(idx, material)| (material, idx as _))
            .collect::<HashMap<_, _>>();

        trace!(
            "Document contains {} material{}",
            materials.len(),
            if materials.len() == 1 { "" } else { "s" },
        );

        let shadow = self.shadow();

        // Build a Mesh from the parts in this document
        let mut primitives = Vec::with_capacity(parts.len() + (parts.len() * shadow as usize));

        for (material, mut data) in parts {
            let material = materials.get(&material).copied().unwrap_or_default();

            if skin.is_none() {
                data.skin = None;
            }

            if !self.normals() {
                data.normals.clear();
            } else if data.normals.is_empty() {
                data.generate_normals();
            }

            if !self.tangents() {
                data.tangents.clear();
            } else if data.tangents.is_empty() {
                warn!(
                    "Tangent data requested but not found: {} (will generate)",
                    self.src().display()
                );

                if data.normals.is_empty() {
                    data.generate_normals();
                }

                if data.textures.0.is_empty() {
                    // We must generate totally fake texture coordinates too
                    data.textures
                        .0
                        .resize(data.positions.len(), Default::default());
                }

                data.tangents
                    .extend(repeat([0.0; 4]).take(data.positions.len()));

                assert!(mikktspace::generate_tangents(&mut data));
            }

            // Main mesh part
            {
                let (vertex, mut vertex_buf) = data.to_vertex_buf();
                let vertex_stride = vertex.stride();

                self.optimize_mesh(&mut data.indices, &mut vertex_buf, vertex_stride);

                let mut primitive = Primitive::new(material, &vertex_buf, vertex);

                for lod_indices in self.calculate_lods(&data.indices, &vertex_buf, vertex_stride) {
                    primitive.push_lod(&lod_indices);
                }

                primitives.push(primitive);
            }

            // Optional shadow mesh part
            if shadow {
                let (vertex, mut vertex_buf) = data.to_shadow_buf();
                let vertex_stride = vertex.stride();

                self.optimize_mesh(&mut data.indices, &mut vertex_buf, vertex_stride);

                let mut primitive = Primitive::new(material, &vertex_buf, vertex);

                for lod_indices in self.calculate_lods(&data.indices, &vertex_buf, vertex_stride) {
                    primitive.push_lod(&lod_indices);
                }

                primitives.push(primitive);
            }
        }

        Ok(Mesh::new(primitives, skin))
    }

    fn re_run_if_changed(&self) {
        // Watch the unstructered data file for changes, only if we're in a cargo build
        if let Some(data) = self.data() {
            re_run_if_changed(data);
        }

        // Watch the GLTF file for changes, only if we're in a cargo build
        let src = self.src();
        re_run_if_changed(src);

        // Just in case there is a GLTF bin file; also watch it for changes
        let mut src_bin = src.to_path_buf();
        src_bin.set_extension("bin");
        re_run_if_changed(src_bin);
    }
}

impl Canonicalize for MeshAsset {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        if let Some(data) = &self.data {
            self.data = Some(Self::canonicalize_project_path(
                &project_dir,
                &src_dir,
                data,
            ));
        }

        self.src = Self::canonicalize_project_path(project_dir, src_dir, &self.src);
    }
}

/// Three-axis scale array or a single value.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum Scale {
    /// An x-y-z scale array.
    Array([OrderedFloat<f32>; 3]),

    /// A single value.
    Value(OrderedFloat<f32>),
}

impl Scale {
    fn de<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ScaleVisitor;

        impl<'de> Visitor<'de> for ScaleVisitor {
            type Value = Option<Scale>;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("floating point sequence or value")
            }

            fn visit_f64<E>(self, val: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let val = val as f32;
                match val.classify() {
                    FpCategory::Zero | FpCategory::Normal => (),
                    _ => panic!("Unexpected scalar value"),
                }

                Ok(Some(Scale::Value(OrderedFloat(val))))
            }

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let val: Vec<f32> = Deserialize::deserialize(SeqAccessDeserializer::new(seq))?;

                if val.len() != 3 {
                    panic!("Unexpected sequence length");
                }

                for val in &val {
                    match val.classify() {
                        FpCategory::Zero | FpCategory::Normal => (),
                        _ => panic!("Unexpected sequence value"),
                    }
                }

                Ok(Some(Scale::Array([
                    OrderedFloat(val[0]),
                    OrderedFloat(val[1]),
                    OrderedFloat(val[2]),
                ])))
            }
        }

        deserializer.deserialize_any(ScaleVisitor)
    }
}

struct VertexData {
    indices: Vec<u32>,
    normals: Vec<[f32; 3]>,
    positions: Vec<[f32; 3]>,
    skin: Option<(Vec<u32>, Vec<u32>)>,
    tangents: Vec<[f32; 4]>,
    textures: (Vec<[f32; 2]>, Vec<[f32; 2]>),
}

impl VertexData {
    fn generate_normals(&mut self) {
        self.normals.clear();
        self.normals
            .resize(self.positions.len(), Default::default());

        for idx in 0..self.indices.len() / 3 {
            let offset = idx * 3;
            let indices = [
                self.indices[offset] as usize,
                self.indices[offset + 1] as usize,
                self.indices[offset + 2] as usize,
            ];
            let vertices = [
                Vec3::from_array(self.positions[indices[0]]),
                Vec3::from_array(self.positions[indices[1]]),
                Vec3::from_array(self.positions[indices[2]]),
            ];

            let normal = (vertices[1] - vertices[0])
                .cross(vertices[2] - vertices[0])
                .normalize();
            self.normals[indices[0]] =
                (Vec3::from_array(self.normals[indices[0]]) + normal).to_array();
            self.normals[indices[1]] =
                (Vec3::from_array(self.normals[indices[1]]) + normal).to_array();
            self.normals[indices[2]] =
                (Vec3::from_array(self.normals[indices[2]]) + normal).to_array();
        }

        for idx in 0..self.normals.len() {
            self.normals[idx] = Vec3::from_array(self.normals[idx]).normalize().to_array();
        }
    }

    fn index(&self, face: usize, vert: usize) -> usize {
        self.indices[face * 3 + vert] as _
    }

    fn to_vertex_buf(&self) -> (VertexType, Vec<u8>) {
        let mut vertex_type = VertexType::POSITION;

        if !self.normals.is_empty() {
            vertex_type |= VertexType::NORMAL;
        }

        if self.skin.is_some() {
            vertex_type |= VertexType::JOINTS_WEIGHTS;
        }

        if !self.tangents.is_empty() {
            vertex_type |= VertexType::TANGENT;
        }

        if !self.textures.0.is_empty() {
            vertex_type |= VertexType::TEXTURE0;
        }

        if !self.textures.1.is_empty() {
            vertex_type |= VertexType::TEXTURE1;
        }

        let vertex_stride = vertex_type.stride();
        let buf_len = self.positions.len() * vertex_stride;
        let mut buf = Vec::with_capacity(buf_len);

        for idx in 0..self.positions.len() {
            let position = self.positions[idx];
            buf.extend_from_slice(&position[0].to_ne_bytes());
            buf.extend_from_slice(&position[1].to_ne_bytes());
            buf.extend_from_slice(&position[2].to_ne_bytes());

            if vertex_type.contains(VertexType::NORMAL) {
                let normal = self.normals[idx];
                buf.extend_from_slice(&normal[0].to_ne_bytes());
                buf.extend_from_slice(&normal[1].to_ne_bytes());
                buf.extend_from_slice(&normal[2].to_ne_bytes());
            }

            if vertex_type.contains(VertexType::TEXTURE0) {
                let textures = self.textures.0[idx];
                buf.extend_from_slice(&textures[0].to_ne_bytes());
                buf.extend_from_slice(&textures[1].to_ne_bytes());
            }

            if vertex_type.contains(VertexType::TEXTURE1) {
                let textures = self.textures.1[idx];
                buf.extend_from_slice(&textures[0].to_ne_bytes());
                buf.extend_from_slice(&textures[1].to_ne_bytes());
            }

            if vertex_type.contains(VertexType::TANGENT) {
                let tangent = self.tangents[idx];
                buf.extend_from_slice(&tangent[0].to_ne_bytes());
                buf.extend_from_slice(&tangent[1].to_ne_bytes());
                buf.extend_from_slice(&tangent[2].to_ne_bytes());
                buf.extend_from_slice(&tangent[3].to_ne_bytes());
            }

            if vertex_type.contains(VertexType::JOINTS_WEIGHTS) {
                let skin = self.skin.as_ref().unwrap();

                let joints = skin.0[idx];
                buf.extend_from_slice(&joints.to_ne_bytes());

                let weights = skin.1[idx];
                buf.extend_from_slice(&weights.to_ne_bytes());
            }

            assert_eq!(buf.len() % vertex_stride, 0);
        }

        assert_eq!(buf.len(), buf_len);

        (vertex_type, buf)
    }

    fn to_shadow_buf(&self) -> (VertexType, Vec<u8>) {
        let mut vertex_type = VertexType::POSITION;

        if self.skin.is_some() {
            vertex_type |= VertexType::JOINTS_WEIGHTS;
        }

        let vertex_stride = vertex_type.stride();
        let buf_len = self.positions.len() * vertex_stride;
        let mut buf = Vec::with_capacity(buf_len);

        for idx in 0..self.positions.len() {
            let position = self.positions[idx];
            buf.extend_from_slice(&position[0].to_ne_bytes());
            buf.extend_from_slice(&position[1].to_ne_bytes());
            buf.extend_from_slice(&position[2].to_ne_bytes());

            if vertex_type.contains(VertexType::JOINTS_WEIGHTS) {
                let skin = self.skin.as_ref().unwrap();

                let joints = skin.0[idx];
                buf.extend_from_slice(&joints.to_ne_bytes());

                let weights = skin.0[idx];
                buf.extend_from_slice(&weights.to_ne_bytes());
            }
        }

        (vertex_type, buf)
    }

    fn transform(&mut self, transform: Mat4) {
        let (_scale, rotation, _translation) = transform.to_scale_rotation_translation();

        for position in &mut self.positions {
            let position4 = Vec3::from_slice(position).extend(1.0);
            position.copy_from_slice(&transform.mul_vec4(position4).to_array()[0..3]);
        }

        for normal in &mut self.normals {
            *normal = rotation.mul_vec3(Vec3::from_array(*normal)).to_array();
        }
    }
}

impl mikktspace::Geometry for VertexData {
    fn num_faces(&self) -> usize {
        self.indices.len() / 3
    }

    fn num_vertices_of_face(&self, _face: usize) -> usize {
        3
    }

    fn position(&self, face: usize, vert: usize) -> [f32; 3] {
        self.positions[self.index(face, vert)]
    }

    fn normal(&self, face: usize, vert: usize) -> [f32; 3] {
        self.normals[self.index(face, vert)]
    }

    fn tex_coord(&self, face: usize, vert: usize) -> [f32; 2] {
        self.textures.0[self.index(face, vert)]
    }

    fn set_tangent_encoded(&mut self, tangent: [f32; 4], face: usize, vert: usize) {
        let idx = self.index(face, vert);
        self.tangents[idx] = tangent;
    }
}
