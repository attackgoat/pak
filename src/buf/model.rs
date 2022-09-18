use {
    super::{
        super::model::{Mesh, ModelBuf, Primitive, Vertex},
        re_run_if_changed, Canonicalize,
    },
    glam::{quat, vec3, EulerRot, Mat4, Quat, Vec3},
    gltf::{
        buffer::Data,
        mesh::{util::ReadIndices, Mode, Reader},
        Buffer, Node,
    },
    log::warn,
    meshopt::{
        generate_vertex_remap, optimize_overdraw_in_place, optimize_vertex_cache_in_place,
        quantize_unorm, remap_index_buffer, simplify, unstripify, VertexDataAdapter,
    },
    ordered_float::OrderedFloat,
    serde::Deserialize,
    std::{
        collections::HashMap,
        path::{Path, PathBuf},
        u16,
    },
};

#[cfg(feature = "bake")]
use {
    super::{file_key, ModelId, Writer},
    gltf::import,
    log::{info, trace},
    parking_lot::Mutex,
    std::{
        collections::HashSet,
        io::{Error, ErrorKind},
        sync::Arc,
    },
};

/// Holds a description of individual meshes within a `.glb` or `.gltf` 3D model.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct MeshRef {
    name: String,
    rename: Option<String>,
}

impl MeshRef {
    /// The artist-provided name of a mesh within the model.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Allows the artist-provided name to be different when referenced by a program.
    pub fn rename(&self) -> Option<&str> {
        let rename = self.rename.as_deref();
        if matches!(rename, Some(rename) if rename.trim().is_empty()) {
            None
        } else {
            rename
        }
    }
}

/// Holds a description of `.glb` or `.gltf` 3D models.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Model {
    lod: Option<bool>,
    lod_target_error: Option<OrderedFloat<f32>>,
    min_lod_triangles: Option<usize>,
    offset: Option<[OrderedFloat<f32>; 3]>,
    optimize: Option<bool>,
    overdraw_threshold: Option<OrderedFloat<f32>>,
    rotation: Option<[OrderedFloat<f32>; 3]>,
    scale: Option<[OrderedFloat<f32>; 3]>,
    shadow: Option<bool>,
    src: PathBuf,

    // Tables must follow values
    #[serde(rename = "mesh")]
    meshes: Option<Vec<MeshRef>>,
}

impl Model {
    pub const DEFAULT_LOD_MIN: usize = 64;
    pub const DEFAULT_LOD_TARGET_ERROR: f32 = 0.05;

    pub fn new(src: impl AsRef<Path>) -> Self {
        Self {
            lod: None,
            lod_target_error: None,
            meshes: None,
            min_lod_triangles: None,
            offset: None,
            optimize: None,
            overdraw_threshold: None,
            rotation: None,
            scale: None,
            shadow: None,
            src: src.as_ref().to_path_buf(),
        }
    }

    /// Reads and processes 3D model source files into an existing `.pak` file buffer.
    #[cfg(feature = "bake")]
    pub fn bake(
        &self,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        path: Option<impl AsRef<Path>>,
    ) -> Result<ModelId, Error> {
        // Early-out if we have already baked this model
        let asset = self.clone().into();
        if let Some(id) = writer.lock().ctx.get(&asset) {
            return Ok(id.as_model().unwrap());
        }

        self.re_run_if_changed();

        // If a path is given it will be available as a key inside the .pak (paths are not
        // given if the asset is specified inline - those are only available in the .pak via ID)
        let key = path.as_ref().map(|path| file_key(&project_dir, &path));
        if let Some(key) = &key {
            // This model will be accessible using this key
            info!("Baking model: {}", key);
        } else {
            // This model will only be accessible using the handle
            info!(
                "Baking model: {} (inline)",
                file_key(&project_dir, self.src())
            );
        }

        let model = self
            .to_model_buf()
            .map_err(|err| Error::new(ErrorKind::InvalidData, err))?;

        // Check again to see if we are the first one to finish this
        let mut writer = writer.lock();
        if let Some(id) = writer.ctx.get(&asset) {
            return Ok(id.as_model().unwrap());
        }

        Ok(writer.push_model(model, key))
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

        loop {
            let target_count = (res.last().unwrap().len() / 3) >> 1;
            if target_count < min_triangles {
                break;
            }

            let lod = simplify(indices, &vertices, target_count, target_error);
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

    /// When `true` levels of detail will be generated for all meshes.
    pub fn lod(&self) -> bool {
        self.lod.unwrap_or_default()
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

    fn node_transform(&self, node: &Node) -> Option<Mat4> {
        let (translation, rotation, scale) = node.transform().decomposed();
        let rotation = quat(rotation[0], rotation[1], rotation[2], rotation[3]);
        let scale = vec3(scale[0], scale[1], scale[2]);
        let translation = vec3(translation[0], translation[1], translation[2]);

        let transform =
            Mat4::from_scale_rotation_translation(self.scale(), self.rotation(), self.offset())
                * Mat4::from_scale_rotation_translation(scale, rotation, translation);

        if transform == Mat4::IDENTITY {
            Some(transform)
        } else {
            None
        }
    }

    /// Translation of the model origin.
    pub fn offset(&self) -> Vec3 {
        self.offset
            .map(|offset| vec3(offset[0].0, offset[1].0, offset[2].0))
            .unwrap_or(Vec3::ZERO)
    }

    /// When `true` this model will be optmizied using the meshopt library.
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

    fn read_bones(node: &Node, bufs: &[Data]) -> HashMap<String, Mat4> {
        node.skin()
            .map(|skin| {
                let joints = skin
                    .joints()
                    .map(|node| node.name().unwrap_or_default().to_owned());
                let inv_binds = skin
                    .reader(|buf| bufs.get(buf.index()).map(|data| data.0.as_slice()))
                    .read_inverse_bind_matrices()
                    .map(|ibp| {
                        ibp.map(|ibp| Mat4::from_cols_array_2d(&ibp))
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();

                joints.zip(inv_binds).into_iter().collect()
            })
            .unwrap_or_default()
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

        let tex_coords = {
            let tex_coords0 = data
                .read_tex_coords(0)
                .map(|data| data.into_f32())
                .map(|tex_coords| tex_coords.collect::<Vec<_>>());

            if tex_coords0.is_none() {
                warn!("Missing texture coordinates!");
            }

            let mut tex_coords0 = tex_coords0.unwrap_or_default();
            tex_coords0.resize(positions.len(), Default::default());

            let mut tex_coords1 = data
                .read_tex_coords(1)
                .map(|data| data.into_f32())
                .map(|tex_coords| tex_coords.collect::<Vec<_>>())
                .unwrap_or_default();

            if !tex_coords1.is_empty() {
                tex_coords1.resize(positions.len(), Default::default());
            }

            (tex_coords0, tex_coords1)
        };

        let normals = {
            let normals = data
                .read_normals()
                .map(|normals| normals.collect::<Vec<_>>());

            if normals.is_none() {
                warn!("Missing normals!");
            }

            let mut normals = normals.unwrap_or_default();
            normals.resize(positions.len(), Default::default());

            normals
        };

        let tangents = {
            let tangents = data
                .read_tangents()
                .map(|tangents| tangents.collect::<Vec<_>>());

            if tangents.is_none() {
                warn!("Missing tangents!");
            }

            let mut tangents = tangents.unwrap_or_default();
            tangents.resize(positions.len(), Default::default());

            tangents
        };

        let joints = data
            .read_joints(0)
            .map(|joints| {
                let mut res = joints
                    .into_u16()
                    .map(|joints| {
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
                tex_coords,
            },
        )
    }

    /// Orientation of the model.
    pub fn rotation(&self) -> Quat {
        let rotation = self.rotation.unwrap_or_default();

        Quat::from_euler(EulerRot::YXZ, rotation[0].0, rotation[1].0, rotation[2].0)
    }

    /// Scaling of the model.
    pub fn scale(&self) -> Vec3 {
        self.scale
            .map(|scale| vec3(scale[0].0, scale[1].0, scale[2].0))
            .unwrap_or(Vec3::ONE)
    }

    /// When `true` position-only shadow meshes will be generated.
    ///
    /// Note: Skinned meshes will contain position, joints, and weights.
    pub fn shadow(&self) -> bool {
        self.shadow.unwrap_or_default()
    }

    /// The model file source.
    pub fn src(&self) -> &Path {
        self.src.as_path()
    }

    #[cfg(feature = "bake")]
    fn to_model_buf(&self) -> gltf::Result<ModelBuf> {
        // Gather a map of the importable mesh names and the renamed name they should get
        let mut mesh_names = HashMap::<_, _>::default();
        if let Some(meshes) = &self.meshes {
            for mesh in meshes {
                mesh_names
                    .entry(mesh.name())
                    .and_modify(|_| warn!("Duplicate mesh name: {}", mesh.name()))
                    .or_insert_with(|| mesh.rename());
            }
        }

        trace!(
            "{} mesh names specified",
            self.meshes
                .as_ref()
                .map(|meshes| meshes.len())
                .unwrap_or_default()
        );

        // Load the mesh nodes from this GLTF file
        let (doc, bufs, _) = import(self.src())?;
        let meshes = doc
            .nodes()
            .filter(|node| {
                mesh_names.is_empty()
                    || node
                        .name()
                        .map(|name| mesh_names.contains_key(name))
                        .unwrap_or_default()
            })
            .filter_map(|node| {
                node.mesh()
                    .map(|mesh| {
                        (
                            mesh.primitives()
                                .filter_map(|primitive| match primitive.mode() {
                                    Mode::TriangleFan | Mode::TriangleStrip | Mode::Triangles => {
                                        trace!(
                                            "Reading mesh \"{}\"",
                                            node.name().unwrap_or_default()
                                        );

                                        // Read material and vertex data
                                        let material =
                                            primitive.material().index().unwrap_or_default();
                                        let (restart_index, mut vertices) =
                                            Self::read_vertices(primitive.reader(|buf| {
                                                bufs.get(buf.index()).map(|data| data.0.as_slice())
                                            }));

                                        // Convert unsupported modes (meshopt requires triangles)
                                        match primitive.mode() {
                                            Mode::TriangleFan => {
                                                Self::convert_triangle_fan_to_list(
                                                    &mut vertices.indices,
                                                )
                                            }
                                            Mode::TriangleStrip => {
                                                Self::convert_triangle_strip_to_list(
                                                    &mut vertices.indices,
                                                    restart_index,
                                                )
                                            }
                                            _ => (),
                                        }

                                        Some((material, vertices))
                                    }
                                    _ => None,
                                })
                                .collect::<Vec<_>>(),
                            node,
                        )
                    })
                    .filter(|(primitives, ..)| !primitives.is_empty())
            })
            .collect::<Vec<_>>();

        // Figure out which unique materials are used on these target mesh primitives and convert
        // those to a map of "Mesh Local" material index from "Gltf File" material index
        // This makes the final materials used index as 0, 1, 2, etc
        let materials = meshes
            .iter()
            .flat_map(|(primitives, ..)| primitives)
            .map(|(material, ..)| *material)
            .collect::<HashSet<_>>()
            .into_iter()
            .enumerate()
            .map(|(idx, material)| (material, idx as _))
            .collect::<HashMap<_, _>>();

        trace!(
            "Document contains {} mesh{} ({} material{})",
            meshes.len(),
            if meshes.len() == 1 { "" } else { "es" },
            materials.len(),
            if materials.len() == 1 { "" } else { "s" },
        );

        let shadow = self.shadow();

        // Build a ModelBuf from the meshes in this document
        let mut model = ModelBuf::default();
        for (primitives, node) in meshes {
            let name = if mesh_names.is_empty() {
                node.name().map(|name| name.to_owned())
            } else {
                mesh_names
                    .get(node.name().unwrap_or_default())
                    .map(|name| name.map(|name| name.to_owned()))
                    .unwrap_or(None)
            };

            trace!(
                "Mesh \"{}\" -> \"{}\"",
                node.name().unwrap_or_default(),
                name.as_deref().unwrap_or_default()
            );

            let bones = Self::read_bones(&node, &bufs);

            // Build a MeshBuf from the primitives in this node
            let mut mesh = Mesh::default();

            if !bones.is_empty() {
                mesh.set_bones(bones);
            }

            if let Some(name) = name {
                mesh.set_name(name);
            }

            if let Some(transform) = self.node_transform(&node) {
                mesh.set_transform(transform);
            }

            for (material, mut data) in primitives {
                let material = materials.get(&material).copied().unwrap_or_default();

                // Main mesh primitive
                {
                    let (vertex, mut vertex_buf) = data.to_vertex_buf();
                    let vertex_stride = vertex.stride();

                    self.optimize_mesh(&mut data.indices, &mut vertex_buf, vertex_stride);

                    let mut primitive = Primitive::new(material, &vertex_buf, vertex);

                    for lod_indices in
                        self.calculate_lods(&data.indices, &vertex_buf, vertex_stride)
                    {
                        primitive.push_lod(&lod_indices);
                    }

                    mesh.push_primitive(primitive);
                }

                // Optional shadow mesh primitive
                if shadow {
                    let (vertex, mut vertex_buf) = data.to_shadow_buf();
                    let vertex_stride = vertex.stride();

                    self.optimize_mesh(&mut data.indices, &mut vertex_buf, vertex_stride);

                    let mut primitive = Primitive::new(material, &vertex_buf, vertex);

                    for lod_indices in
                        self.calculate_lods(&data.indices, &vertex_buf, vertex_stride)
                    {
                        primitive.push_lod(&lod_indices);
                    }

                    mesh.push_primitive(primitive);
                }
            }

            model.push_mesh(mesh);
        }

        Ok(model)
    }

    fn re_run_if_changed(&self) {
        // Watch the GLTF file for changes, only if we're in a cargo build
        let src = self.src();
        re_run_if_changed(&src);

        // Just in case there is a GLTF bin file; also watch it for changes
        let mut src_bin = src.to_path_buf();
        src_bin.set_extension("bin");
        re_run_if_changed(src_bin);
    }
}

impl Canonicalize for Model {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        self.src = Self::canonicalize_project_path(project_dir, src_dir, &self.src);
    }
}

struct VertexData {
    indices: Vec<u32>,
    normals: Vec<[f32; 3]>,
    positions: Vec<[f32; 3]>,
    skin: Option<(Vec<u32>, Vec<u32>)>,
    tangents: Vec<[f32; 4]>,
    tex_coords: (Vec<[f32; 2]>, Vec<[f32; 2]>),
}

impl VertexData {
    fn to_vertex_buf(&self) -> (Vertex, Vec<u8>) {
        let mut vertex = Vertex::POSITION | Vertex::NORMAL_TANGENT_TEX_COORD0;

        if self.skin.is_some() {
            vertex |= Vertex::JOINTS_WEIGHTS;
        }

        if !self.tex_coords.1.is_empty() {
            vertex |= Vertex::TEX_COORD1;
        }

        let vertex_stride = vertex.stride();
        let buf_len = self.positions.len() * vertex_stride;
        let mut buf = Vec::with_capacity(buf_len);

        for idx in 0..self.positions.len() {
            let position = self.positions[idx];
            buf.extend_from_slice(&position[0].to_ne_bytes());
            buf.extend_from_slice(&position[1].to_ne_bytes());
            buf.extend_from_slice(&position[2].to_ne_bytes());

            let tex_coord = self.tex_coords.0[idx];
            buf.extend_from_slice(&tex_coord[0].to_ne_bytes());
            buf.extend_from_slice(&tex_coord[1].to_ne_bytes());

            if !self.tex_coords.1.is_empty() {
                let tex_coord = self.tex_coords.1[idx];
                buf.extend_from_slice(&tex_coord[0].to_ne_bytes());
                buf.extend_from_slice(&tex_coord[1].to_ne_bytes());
            }

            let normal = self.normals[idx];
            buf.extend_from_slice(&normal[0].to_ne_bytes());
            buf.extend_from_slice(&normal[1].to_ne_bytes());
            buf.extend_from_slice(&normal[2].to_ne_bytes());

            let tangent = self.tangents[idx];
            buf.extend_from_slice(&tangent[0].to_ne_bytes());
            buf.extend_from_slice(&tangent[1].to_ne_bytes());
            buf.extend_from_slice(&tangent[2].to_ne_bytes());
            buf.extend_from_slice(&tangent[3].to_ne_bytes());

            if let Some(skin) = &self.skin {
                let joints = skin.0[idx];
                buf.extend_from_slice(&joints.to_ne_bytes());

                let weights = skin.1[idx];
                buf.extend_from_slice(&weights.to_ne_bytes());
            }

            assert_eq!(buf.len() % vertex_stride, 0);
        }

        assert_eq!(buf.len(), buf_len);

        (vertex, buf)
    }

    fn to_shadow_buf(&self) -> (Vertex, Vec<u8>) {
        let mut vertex = Vertex::POSITION;

        if self.skin.is_some() {
            vertex |= Vertex::JOINTS_WEIGHTS;
        }

        let vertex_stride = vertex.stride();
        let buf_len = self.positions.len() * vertex_stride;
        let mut buf = Vec::with_capacity(buf_len);

        for idx in 0..self.positions.len() {
            let position = self.positions[idx];
            buf.extend_from_slice(&position[0].to_ne_bytes());
            buf.extend_from_slice(&position[1].to_ne_bytes());
            buf.extend_from_slice(&position[2].to_ne_bytes());

            if let Some(skin) = &self.skin {
                let joints = skin.0[idx];
                buf.extend_from_slice(&joints.to_ne_bytes());

                let weights = skin.0[idx];
                buf.extend_from_slice(&weights.to_ne_bytes());
            }
        }

        (vertex, buf)
    }
}
