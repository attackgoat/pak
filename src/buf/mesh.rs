pub mod lod;

use {
    super::{Canonicalize, Euler, Rotation, Writer, blob::BlobAsset, file_key, re_run_if_changed},
    crate::{
        MeshId,
        index::IndexBuffer,
        mesh::{Geometry, Joint, Mesh, Primitive, Skin, VertexType},
    },
    anyhow::{Context, bail, ensure},
    glam::{EulerRot, Mat4, Quat, Vec3, Vec4, vec3},
    gltf::{
        Buffer, Node,
        buffer::Data,
        import,
        mesh::{Mode, Reader, util::ReadIndices},
    },
    log::{info, trace, warn},
    meshopt::{
        SimplifyOptions, VertexDataAdapter, optimize_overdraw_in_place,
        optimize_vertex_cache_in_place, quantize_unorm, remap_index_buffer, simplify,
        simplify_sloppy, unstripify,
    },
    ordered_float::OrderedFloat,
    parking_lot::Mutex,
    serde::{
        Deserialize, Deserializer,
        de::{SeqAccess, Visitor, value::SeqAccessDeserializer},
    },
    std::{
        collections::{BTreeMap, BTreeSet, HashMap, HashSet},
        fmt::Formatter,
        io::{Error, ErrorKind},
        iter::repeat_n,
        num::FpCategory,
        path::{Path, PathBuf},
        sync::Arc,
    },
};

fn extract_transform(node: &Node) -> Mat4 {
    let (translation, rotation, scale) = node.transform().decomposed();
    let translation = Vec3::from_array(translation);
    let rotation = Quat::from_array(rotation);
    let scale = Vec3::from_array(scale);

    Mat4::from_scale_rotation_translation(scale, rotation, translation)
}

#[cfg(test)]
use crate::mesh::Lod;

/// Holds a description of `.glb` or `.gltf` 3D meshes.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct MeshAsset {
    blob: Option<PathBuf>,
    pub data: Option<BTreeMap<String, super::scene::Data>>,
    euler: Option<Euler>,
    flip_x: Option<bool>,
    flip_y: Option<bool>,
    flip_z: Option<bool>,
    ignore_skin: Option<bool>,
    ignore_texture1: Option<bool>,
    inherit_lods: Option<bool>,
    #[serde(default)]
    lod: lod::LodSettings,
    lods: Option<Box<[lod::LodRequest]>>,
    lod_lock_border: Option<bool>,
    lod_target_error: Option<OrderedFloat<f32>>,
    max_index: Option<MaxIndex>,
    min_lod_triangles: Option<usize>,

    /// The artist-provided name of a mesh within the source file.
    name: Option<String>,

    normals: Option<bool>,
    offset: Option<[OrderedFloat<f32>; 3]>,
    optimize: Option<bool>,
    optimize_vertex_cache: Option<bool>,
    overdraw_threshold: Option<OrderedFloat<f32>>,

    rotation: Option<Rotation>,

    #[serde(default, deserialize_with = "Scale::de")]
    scale: Option<Scale>,

    scene_name: Option<String>,
    src: Option<PathBuf>,
    tangents: Option<bool>,
}

impl MeshAsset {
    pub const DEFAULT_LOD_MIN: usize = 64;
    pub const DEFAULT_LOD_TARGET_ERROR: f32 = 0.05;

    pub fn new(src: impl AsRef<Path>) -> Self {
        Self {
            blob: None,
            data: None,
            euler: None,
            flip_x: None,
            flip_y: None,
            flip_z: None,
            ignore_skin: None,
            ignore_texture1: None,
            inherit_lods: None,
            lod: lod::LodSettings::default(),
            lods: None,
            lod_lock_border: None,
            lod_target_error: None,
            max_index: None,
            min_lod_triangles: None,
            name: None,
            normals: None,
            offset: None,
            optimize: None,
            optimize_vertex_cache: None,
            overdraw_threshold: None,
            rotation: None,
            scale: None,
            scene_name: None,
            src: Some(src.as_ref().to_path_buf()),
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
        let src = self
            .src()
            .ok_or(anyhow::Error::msg("unspecified mesh source"))?;

        // Early-out if we have already baked this mesh
        let asset = self.clone().into();
        let key = path.as_ref().map(|path| file_key(&project_dir, path));

        if let Some(id) = writer.lock().asset_id(&asset, key.as_deref())? {
            return id.as_mesh().context("asset context returned non-mesh id");
        }

        self.re_run_if_changed();

        // If a path is given it will be available as a key inside the .pak (paths are not
        // given if the asset is specified inline - those are only available in the .pak via ID)
        if let Some(key) = &key {
            // This mesh will be accessible using this key
            info!("Baking mesh: {}", key);
        } else {
            // This mesh will only be accessible using the handle
            info!("Baking mesh: {} (inline)", file_key(&project_dir, src));
        }

        // Resolve pack defaults only for import; Writer keys and policies use the raw recipe.
        let mut effective = self.clone();
        {
            let writer = writer.lock();
            effective.lods = Some(self.resolve_lod_requests(&writer.default_lods)?);
            effective.lod.inherit(&writer.lod);
        }
        let mut mesh = effective
            .to_mesh(src)
            .map_err(|err| Error::new(ErrorKind::InvalidData, err))
            .context("Baking mesh data")?;

        // Bake the unstructured data blob too
        if let Some(data) = &self.blob {
            let data_id = Writer::with_asset_policy(writer, &asset, || {
                BlobAsset::new(data)
                    .bake(writer, &project_dir)
                    .context("Baking unstructured mesh data")
            })?;
            mesh.set_blob(data_id);
        }

        // Check again to see if we are the first one to finish this
        let mut writer = writer.lock();
        if let Some(id) = writer.asset_id(&asset, key.as_deref())? {
            return id.as_mesh().context("asset context returned non-mesh id");
        }

        let policy = writer.policy_for(&asset);
        let id = writer.push_mesh(mesh, policy)?;
        writer.commit_asset(asset, id, key)?;

        Ok(id)
    }

    #[cfg(test)]
    fn calculate_lods(&self, source: Geometry) -> anyhow::Result<Box<[Lod]>> {
        Ok(self.clustered_lods(source, &BTreeSet::new())?.0)
    }

    #[cfg(test)]
    fn layout_lods(
        &self,
        source: &Geometry,
        layout: VertexType,
    ) -> anyhow::Result<crate::mesh::LodSet> {
        lod::LodRequest {
            layout,
            simplify: !layout.contains(VertexType::TEXTURE0),
        }
        .process(source.project(layout)?, &self.resolved_lod_settings()?, &[])
    }

    #[cfg(test)]
    pub(super) fn generate_lods(&self, primitive: &mut Primitive) -> anyhow::Result<()> {
        self.generate_lods_with_seams(primitive, &BTreeSet::new())
            .map(|_| ())
    }

    fn generate_lods_with_seams(
        &self,
        primitive: &mut Primitive,
        seams: &BTreeSet<lod::Position>,
    ) -> anyhow::Result<()> {
        let settings = self.resolved_lod_settings()?;
        let locks = seams
            .iter()
            .map(|position| position.map(f32::from_bits))
            .collect::<Vec<_>>();
        let mut layouts = BTreeSet::new();
        let mut sets = Vec::new();
        for request in self.lod_requests() {
            ensure!(
                layouts.insert(request.layout),
                "duplicate lod vertex layout request"
            );
            sets.push(request.process(
                primitive.base().project(request.layout)?,
                &settings,
                &locks,
            )?);
        }
        primitive.set_lods(sets.into_boxed_slice())
    }

    fn convert_triangle_fan_to_list(indices: &mut Vec<u32>) {
        if indices.len() < 4 {
            return;
        }

        let anchor = indices[0];
        let mut result = Vec::with_capacity((indices.len() - 2) * 3);
        result.extend_from_slice(&indices[0..3]);
        for i in 3..indices.len() {
            result.push(anchor);
            result.push(indices[i - 1]);
            result.push(indices[i]);
        }

        *indices = result;
    }

    fn convert_triangle_strip_to_list(
        indices: &mut Vec<u32>,
        restart_index: u32,
    ) -> anyhow::Result<()> {
        ensure!(
            indices.len() >= 3,
            "triangle strip must have at least 3 indices"
        );
        *indices =
            unstripify(indices, restart_index).context("unable to unstripify index buffer")?;
        Ok(())
    }

    /// Optional associated unstructured data.
    pub fn blob(&self) -> Option<&Path> {
        self.blob.as_deref()
    }

    /// Whether to include pack default LOD requests. Defaults to `true`.
    pub fn inherit_lods(&self) -> bool {
        self.inherit_lods.unwrap_or(true)
    }

    /// Local layout requests, merged with pack defaults when inheritance is enabled.
    /// Local requests override matching default layouts; an empty list adds no overrides.
    pub fn lod_requests(&self) -> &[lod::LodRequest] {
        self.lods.as_deref().unwrap_or_default()
    }

    fn resolve_lod_requests(
        &self,
        defaults: &[lod::LodRequest],
    ) -> anyhow::Result<Box<[lod::LodRequest]>> {
        let mut requests = BTreeMap::new();
        for request in self.lod_requests() {
            ensure!(
                requests.insert(request.layout, request.clone()).is_none(),
                "duplicate lod vertex layout request"
            );
        }

        if self.inherit_lods() {
            for request in defaults {
                requests
                    .entry(request.layout)
                    .or_insert_with(|| request.clone());
            }
        }

        Ok(requests.into_values().collect())
    }

    /// When `true` levels of detail vertices that lie on the topological border of the mesh will be
    /// locked in place such that they don’t move during simplification.
    ///
    /// This can be valuable to simplify independent chunks of a mesh, for example terrain, to
    /// ensure that individual levels of detail can be stitched together later without gaps.
    pub fn lod_lock_border(&self) -> bool {
        self.lod_lock_border.unwrap_or_default()
    }

    /// Relative meshoptimizer geometric error limit, independent of count-reduction acceptance.
    pub fn lod_target_error(&self) -> f32 {
        self.lod_target_error
            .unwrap_or(OrderedFloat(Self::DEFAULT_LOD_TARGET_ERROR))
            .0
    }

    /// The highest index value allowed in the source mesh (before LOD generation).
    pub fn max_index(&self) -> Option<u32> {
        self.max_index.map(MaxIndex::value)
    }

    /// When `true` (the default) normal values will be stored (or generated if needed).
    pub fn normals(&self) -> bool {
        self.normals.unwrap_or(true)
    }

    /// When `true`, the second texture coordinate channel will not be stored.
    pub fn ignore_texture1(&self) -> bool {
        self.ignore_texture1.unwrap_or_default()
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

    /// Reorders triangles for vertex reuse, without changing their winding or vertices.
    /// Defaults to `optimize`; explicitly enabling this with `optimize = false`
    /// leaves overdraw and vertex-fetch reordering disabled.
    pub fn optimize_vertex_cache(&self) -> bool {
        self.optimize_vertex_cache
            .unwrap_or_else(|| self.optimize())
    }

    /// At the very least this function will re-index the vertices, and optionally may
    /// perform full meshopt optimization.
    fn optimize_mesh(
        &self,
        indices: &mut Vec<u32>,
        vertex_buf: &mut Vec<u8>,
        vertex_stride: usize,
    ) -> anyhow::Result<()> {
        Geometry::validate_pair(vertex_buf, vertex_stride, indices)?;
        ensure!(
            self.overdraw_threshold().is_finite() && self.overdraw_threshold() >= 1.0,
            "overdraw-threshold must be finite and at least one"
        );
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
        if self.optimize_vertex_cache() {
            optimize_vertex_cache_in_place(indices, vertex_count);
        }

        if self.optimize() {
            let vertices = VertexDataAdapter::new(vertex_buf, vertex_stride, 0)
                .context("creating vertex data adapter for mesh optimization")?;

            // HACK: These functions take immutable borrows, BUT USE MUTABLE!
            // See: https://github.com/gwihlidal/meshopt-rs/pull/26 not yet released
            optimize_overdraw_in_place(indices, &vertices, self.overdraw_threshold());

            hack::optimize_vertex_fetch_in_place(indices, vertex_buf, vertex_stride);
        }

        Ok(())
    }

    fn compact_mesh_indices(
        indices: &mut [u32],
        vertex_buf: &[u8],
        vertex_stride: usize,
    ) -> anyhow::Result<(Vec<u8>, usize)> {
        ensure!(
            vertex_stride != 0 && vertex_buf.len().is_multiple_of(vertex_stride),
            "invalid compact vertex stride"
        );
        let vertex_count = vertex_buf.len() / vertex_stride;
        let mut remap = vec![None; vertex_count];
        let mut compact = Vec::new();

        for idx in indices {
            let src = *idx as usize;
            if src >= vertex_count {
                bail!("mesh index {} exceeds vertex count {}", src, vertex_count);
            }

            let dst = match remap[src] {
                Some(dst) => dst,
                None => {
                    let dst = compact.len() / vertex_stride;
                    let offset = src * vertex_stride;
                    compact.extend_from_slice(&vertex_buf[offset..offset + vertex_stride]);
                    remap[src] = Some(dst as u32);
                    dst as u32
                }
            };

            *idx = dst;
        }

        let compact_count = compact.len() / vertex_stride;
        Ok((compact, compact_count))
    }

    fn reduce_mesh_to_max_index(
        &self,
        indices: &mut Vec<u32>,
        vertex_buf: &mut Vec<u8>,
        vertex_stride: usize,
    ) -> anyhow::Result<()> {
        Geometry::validate_pair(vertex_buf, vertex_stride, indices)?;
        let Some(max_index) = self.max_index() else {
            return Ok(());
        };

        if indices.iter().copied().max().unwrap_or_default() <= max_index {
            return Ok(());
        }

        if max_index < 2 {
            bail!("max-index must allow at least 3 vertices");
        }

        let max_vertices = max_index as usize + 1;

        let mut compacted_indices = indices.clone();
        let (compacted_vertices, compacted_vertex_count) =
            Self::compact_mesh_indices(&mut compacted_indices, vertex_buf, vertex_stride)?;
        if compacted_vertex_count <= max_vertices {
            *indices = compacted_indices;
            *vertex_buf = compacted_vertices;
            return Ok(());
        }

        let target_error = self.lod_target_error();
        ensure!(
            target_error.is_finite() && target_error >= 0.0,
            "lod-target-error must be finite and nonnegative"
        );
        let opts = if self.lod_lock_border() {
            SimplifyOptions::LockBorder
        } else {
            SimplifyOptions::None
        };

        let fits = |candidate: Vec<u32>| -> anyhow::Result<Option<(Vec<u32>, Vec<u8>)>> {
            if candidate.len() < 3 {
                return Ok(None);
            }

            let mut candidate = candidate;
            let (candidate_vertices, candidate_vertex_count) =
                Self::compact_mesh_indices(&mut candidate, vertex_buf, vertex_stride)?;

            if candidate_vertex_count <= max_vertices {
                Ok(Some((candidate, candidate_vertices)))
            } else {
                Ok(None)
            }
        };

        let initial_target = (indices.len() / 2).min(max_vertices * 3).max(3);
        let mut target_count = initial_target - (initial_target % 3);
        if target_count < 3 {
            target_count = 3;
        }

        while target_count >= 3 {
            let vertices = VertexDataAdapter::new(vertex_buf, vertex_stride, 0)
                .context("creating vertex data adapter for index-limited simplification")?;
            let candidate = simplify(indices, &vertices, target_count, target_error, opts, None);

            if let Some((candidate, candidate_vertices)) = fits(candidate)? {
                *indices = candidate;
                *vertex_buf = candidate_vertices;
                return Ok(());
            }

            if target_count == 3 {
                break;
            }

            target_count = ((target_count / 2) / 3).max(1) * 3;
        }

        let mut target_count = initial_target;
        while target_count >= 3 {
            let vertices = VertexDataAdapter::new(vertex_buf, vertex_stride, 0)
                .context("creating vertex data adapter for index-limited sloppy simplification")?;
            let candidate = simplify_sloppy(indices, &vertices, target_count, target_error, None);

            if let Some((candidate, candidate_vertices)) = fits(candidate)? {
                *indices = candidate;
                *vertex_buf = candidate_vertices;
                return Ok(());
            }

            if target_count == 3 {
                break;
            }

            target_count = ((target_count / 2) / 3).max(1) * 3;
        }

        bail!("unable to reduce mesh to max-index {max_index}")
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
                for joint_name in skin.joints().filter_map(|joint| joint.name()) {
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

    /// Sets the mesh file source.
    pub fn set_src(&mut self, src: impl AsRef<Path>) {
        self.src = Some(src.as_ref().to_path_buf());
    }

    /// Scaling of the mesh.
    pub fn scale(&self) -> Vec3 {
        self.scale
            .map(|scale| match scale {
                Scale::Array([OrderedFloat(x), OrderedFloat(y), OrderedFloat(z)]) => vec3(x, y, z),
                Scale::Value(OrderedFloat(scale)) => Vec3::splat(scale),
            })
            .inspect(|scale| {
                assert!(scale.is_finite(), "scale must be finite");
                assert!(scale.min_element() > 0.0, "scale must be greater than zero");
            })
            .unwrap_or(Vec3::ONE)
    }

    /// The mesh file source.
    pub fn src(&self) -> Option<&Path> {
        self.src.as_deref()
    }

    /// When `true` (the default) tangent values will be stored (or generated if needed).
    pub fn tangents(&self) -> bool {
        self.tangents.unwrap_or(true)
    }

    fn to_mesh(&self, src: impl AsRef<Path>) -> anyhow::Result<Mesh> {
        let settings = self.resolved_lod_settings()?;
        let src = src.as_ref();

        // Load the mesh nodes from this GLTF file
        let (doc, bufs, _) =
            import(src).with_context(|| format!("Importing mesh source: {}", src.display()))?;
        let scene = self
            .scene_name
            .as_deref()
            .and_then(|name| doc.scenes().find(|scene| scene.name() == Some(name)))
            .or_else(|| doc.default_scene())
            .or_else(|| doc.scenes().next())
            .ok_or(anyhow::Error::msg("No scene found"))?;
        let all_nodes = {
            let mut nodes = vec![];
            let mut todo = scene.nodes().collect::<Vec<_>>();

            while let Some(node) = todo.pop() {
                todo.extend(node.children());
                nodes.push(node);
            }

            nodes
        };
        let mut mesh_nodes = all_nodes.iter().filter(|node| node.mesh().is_some());
        let node = self
            .name
            .as_deref()
            .and_then(|name| mesh_nodes.find(|node| node.name() == Some(name)))
            .or_else(|| mesh_nodes.next())
            .ok_or(anyhow::Error::msg("No mesh found"))?;
        let allow_skin = !self.ignore_skin.unwrap_or_default();
        let mesh_transform =
            Mat4::from_scale_rotation_translation(self.scale(), self.rotation(), self.offset());

        info!("Loading mesh {}", node.name().unwrap_or_default());

        let skin = allow_skin
            .then(|| Self::read_skin(node, &bufs, mesh_transform))
            .flatten();
        let transform = mesh_transform * extract_transform(node);
        let parts = node
            .mesh()
            .context("node has no mesh")?
            .primitives()
            .filter(|primitive| {
                matches!(
                    primitive.mode(),
                    Mode::TriangleFan | Mode::TriangleStrip | Mode::Triangles
                )
            })
            .map(|primitive| {
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
                    Mode::TriangleFan => Self::convert_triangle_fan_to_list(&mut vertices.indices),
                    Mode::TriangleStrip => {
                        Self::convert_triangle_strip_to_list(&mut vertices.indices, restart_index)?
                    }
                    _ => (),
                }

                vertices.validate()?;
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

                vertices.validate()?;
                Ok((material, vertices, primitive.morph_targets().len() != 0))
            })
            .collect::<anyhow::Result<Vec<_>>>()?;

        // Figure out which unique materials are used on these target mesh primitives and convert
        // those to a map of "Mesh Local" material index from "Gltf File" material index
        // This makes the final materials used index as 0, 1, 2, etc
        let materials = parts
            .iter()
            .map(|(material, ..)| *material)
            .collect::<BTreeSet<_>>();
        ensure!(
            materials.len() <= u8::MAX as usize + 1,
            "mesh contains too many material slots"
        );
        let materials = materials
            .into_iter()
            .enumerate()
            .map(|(idx, material)| (material, idx as _))
            .collect::<HashMap<_, _>>();

        trace!(
            "Document contains {} material{}",
            materials.len(),
            if materials.len() == 1 { "" } else { "s" },
        );

        // Build a Mesh from the parts in this document
        let mut primitives = Vec::with_capacity(parts.len());
        let mut source_deformation = Vec::with_capacity(parts.len());

        for (material, mut data, deformed) in parts {
            let material = materials.get(&material).copied().unwrap_or_default();

            if skin.is_none() {
                data.skin = None;
            }

            if self.ignore_texture1() {
                data.textures.1.clear();
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
                    src.display()
                );

                data.generate_tangents();
            } else if !data.tangents_are_valid() {
                warn!(
                    "Invalid tangent data found: {} (will regenerate)",
                    src.display()
                );

                data.generate_tangents();
            }

            let (vertex_type, mut vertex_buf) = data.to_vertex_buf();
            vertex_type.validate()?;
            let vertex_stride = vertex_type.stride();
            self.optimize_mesh(&mut data.indices, &mut vertex_buf, vertex_stride)?;
            self.reduce_mesh_to_max_index(&mut data.indices, &mut vertex_buf, vertex_stride)?;
            let base = Geometry::new(&vertex_buf, vertex_type, IndexBuffer::new(&data.indices)?)?;
            let primitive = Primitive::new(material, base)?;
            source_deformation.push(skin.is_some() || node.skin().is_some() || deformed);
            primitives.push(primitive);
        }

        // Material/primitive seams are always protected, independently of external borders.
        let mut owners = BTreeMap::new();
        let mut seams = BTreeSet::new();
        for (idx, primitive) in primitives
            .iter()
            .enumerate()
            .filter(|_| !self.lod_requests().is_empty())
        {
            for vertex in primitive.base().indices().as_u32() {
                let position = lod::position_key(primitive.base().position(vertex));
                if owners
                    .insert(position, idx)
                    .is_some_and(|owner| owner != idx)
                {
                    seams.insert(position);
                }
            }
        }
        for primitive in &mut primitives {
            self.generate_lods_with_seams(primitive, &seams)?;
        }
        let mut mesh = Mesh::new(primitives, skin)?;
        mesh.data = self
            .data
            .iter()
            .flat_map(|data| data.iter())
            .map(|(key, value)| (key.clone(), value.clone().into()))
            .collect();
        ensure!(
            mesh.data.iter().all(
                |(key, _)| !key.starts_with("pak.mesh-lod.") && key != "pak.source-deformation"
            ),
            "mesh data uses reserved geometry namespace"
        );
        mesh.data.insert(
            "pak.mesh-lod.settings",
            crate::scene::DataData::String(toml::to_string(&settings)?),
        );
        mesh.data.insert(
            "pak.mesh-lod.producer",
            crate::scene::DataData::String(super::MESH_LOD_PRODUCER.to_owned()),
        );
        if mesh.primitives().is_empty() {
            mesh.data.insert(
                "pak.mesh-lod.empty-stop",
                crate::scene::DataData::String("no-supported-triangle-primitives".to_owned()),
            );
        }
        mesh.data.insert(
            "pak.source-deformation",
            crate::scene::DataData::Array(
                source_deformation
                    .into_iter()
                    .map(crate::scene::DataData::Bool)
                    .collect(),
            ),
        );
        Ok(mesh)
    }

    fn re_run_if_changed(&self) {
        // Watch the unstructered data file for changes, only if we're in a cargo build
        if let Some(data) = self.blob() {
            re_run_if_changed(data);
        }

        if let Some(src) = &self.src {
            // Watch the GLTF file for changes, only if we're in a cargo build
            re_run_if_changed(src);

            // Just in case there is a GLTF bin file; also watch it for changes
            let mut src_bin = src.to_path_buf();
            src_bin.set_extension("bin");
            re_run_if_changed(src_bin);
        }
    }
}

impl Canonicalize for MeshAsset {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        if let Some(data) = &self.blob {
            self.blob = Some(Self::canonicalize_project_path(
                &project_dir,
                &src_dir,
                data,
            ));
        }

        if let Some(src) = &mut self.src {
            self.src = Some(Self::canonicalize_project_path(project_dir, src_dir, src))
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
enum MaxIndex {
    U8,
    U16,
    Exact(u32),
}

impl MaxIndex {
    fn value(self) -> u32 {
        match self {
            Self::U8 => u8::MAX as u32,
            Self::U16 => u16::MAX as u32,
            Self::Exact(value) => value,
        }
    }
}

impl<'de> Deserialize<'de> for MaxIndex {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct MaxIndexVisitor;

        impl Visitor<'_> for MaxIndexVisitor {
            type Value = MaxIndex;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("u8, u16, or an exact maximum index value")
            }

            fn visit_i64<E>(self, value: i64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let value = u32::try_from(value)
                    .map_err(|_| E::custom("max-index must be between 0 and u32::MAX"))?;
                Ok(MaxIndex::Exact(value))
            }

            fn visit_u64<E>(self, value: u64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let value = u32::try_from(value)
                    .map_err(|_| E::custom("max-index must be between 0 and u32::MAX"))?;
                Ok(MaxIndex::Exact(value))
            }

            fn visit_str<E>(self, value: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                match value.to_ascii_lowercase().as_str() {
                    "u8" => Ok(MaxIndex::U8),
                    "u16" => Ok(MaxIndex::U16),
                    _ => Err(E::unknown_variant(value, &["u8", "u16"])),
                }
            }

            fn visit_string<E>(self, value: String) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                self.visit_str(&value)
            }
        }

        deserializer.deserialize_any(MaxIndexVisitor)
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
                    _ => return Err(E::custom("expected a normal floating point value")),
                }

                Ok(Some(Scale::Value(OrderedFloat(val))))
            }

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let val: Vec<f32> = Deserialize::deserialize(SeqAccessDeserializer::new(seq))?;

                if val.len() != 3 {
                    return Err(serde::de::Error::custom("expected 3 values"));
                }

                for val in &val {
                    match val.classify() {
                        FpCategory::Zero | FpCategory::Normal => (),
                        _ => {
                            return Err(serde::de::Error::custom(
                                "expected a normal floating point value",
                            ));
                        }
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
    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            !self.positions.is_empty() && self.positions.len() <= u32::MAX as usize,
            "invalid mesh position count"
        );
        ensure!(
            self.positions
                .iter()
                .flatten()
                .all(|value| value.is_finite()),
            "mesh positions must be finite"
        );
        ensure!(
            self.indices.len() >= 3
                && self.indices.len().is_multiple_of(3)
                && self.indices.len() <= u32::MAX as usize,
            "mesh indices must be triangles"
        );
        ensure!(
            self.indices
                .iter()
                .all(|&index| (index as usize) < self.positions.len()),
            "mesh index exceeds vertex count"
        );
        Ok(())
    }

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

    fn generate_tangents(&mut self) {
        if self.normals.is_empty() {
            self.generate_normals();
        }

        if self.textures.0.is_empty() {
            self.textures
                .0
                .resize(self.positions.len(), Default::default());
        }

        self.tangents.clear();
        self.tangents
            .extend(repeat_n([0.0; 4], self.positions.len()));

        assert!(mikktspace::generate_tangents(self));

        self.repair_invalid_tangents();

        debug_assert!(self.tangents_are_valid());
    }

    fn repair_invalid_tangents(&mut self) {
        for (tangent, normal) in self.tangents.iter_mut().zip(&self.normals) {
            if !Self::tangent_is_valid(*tangent, *normal) {
                let normal = Vec3::from_array(*normal).normalize_or(Vec3::Y);
                let axis = if normal.x.abs() < 0.9 {
                    Vec3::X
                } else {
                    Vec3::Y
                };
                let fallback = normal.cross(axis).normalize_or(Vec3::Z);
                *tangent = [fallback.x, fallback.y, fallback.z, 1.0];
            }
        }
    }

    fn tangent_is_valid(tangent: [f32; 4], normal: [f32; 3]) -> bool {
        let normal = Vec3::from_array(normal);
        let tangent_vec = Vec4::from_array(tangent).truncate();
        let projected = tangent_vec - normal * tangent_vec.dot(normal);

        tangent[3] != 0.0 && projected.length_squared() > 0.000001
    }

    fn tangents_are_valid(&self) -> bool {
        if self.tangents.len() != self.positions.len() || self.normals.len() != self.positions.len()
        {
            return false;
        }

        self.tangents
            .iter()
            .zip(&self.normals)
            .all(|(tangent, normal)| Self::tangent_is_valid(*tangent, *normal))
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

            if let Some(skin) = self.skin.as_ref() {
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

#[cfg(test)]
mod test {
    use {
        super::{MaxIndex, MeshAsset, VertexData},
        crate::{
            index::IndexBuffer,
            mesh::{
                Geometry, Mesh, Primitive, VertexType,
                test::{grid, triangles},
            },
        },
        meshopt::{SimplifyOptions, VertexDataAdapter, simplify, simplify_scale},
        std::{fs, path::Path},
    };

    const LOD_REQUESTS: &str = "lods = [{layout='POSITION', simplify=true}, {layout='PACKED_NORMAL', simplify=true}, {layout='POSITION | TEXTURE0', simplify=false}, {layout='PACKED_NORMAL | TEXTURE0', simplify=false}]";

    fn assert_compact(geometry: &Geometry) {
        let mut referenced = geometry.indices().as_u32();
        referenced.sort_unstable();
        referenced.dedup();
        assert_eq!(
            referenced,
            (0..geometry.vertex_count() as u32).collect::<Vec<_>>()
        );
    }

    fn assert_full_detail_and_compact_lods(primitive: &Primitive) {
        for set in primitive.lod_sets() {
            assert_eq!(
                triangles(
                    set.levels()[0].geometry(),
                    set.vertex_type().contains(VertexType::TEXTURE0)
                ),
                triangles(
                    primitive.base(),
                    set.vertex_type().contains(VertexType::TEXTURE0)
                )
            );
            for lod in &set.levels()[1..] {
                assert_compact(lod.geometry());
            }
        }
    }

    fn max_abs_position(mesh: &Mesh) -> f32 {
        let mut max = 0.0f32;

        for primitive in mesh.primitives() {
            let stride = primitive.base().vertex_type().stride();

            for vertex in primitive.base().vertex_data().chunks_exact(stride) {
                let x = f32::from_ne_bytes(vertex[0..4].try_into().unwrap());
                let y = f32::from_ne_bytes(vertex[4..8].try_into().unwrap());
                let z = f32::from_ne_bytes(vertex[8..12].try_into().unwrap());

                max = max.max(x.abs()).max(y.abs()).max(z.abs());
            }
        }

        max
    }

    #[test]
    fn lod_requests_merge_defaults_by_layout_and_opt_out_explicitly() {
        let defaults: MeshAsset = toml::from_str(
            "lods = [{layout='POSITION', simplify=true}, {layout='PACKED_NORMAL', simplify=true}]",
        )
        .unwrap();
        let defaults = defaults.lod_requests();
        for recipe in ["", "lods = []", "inherit-lods = true\nlods = []"] {
            let mesh: MeshAsset = toml::from_str(recipe).unwrap();
            assert!(mesh.inherit_lods());
            assert_eq!(&*mesh.resolve_lod_requests(defaults).unwrap(), defaults);
            assert!(mesh.lod_requests().is_empty());
        }

        let mesh: MeshAsset = toml::from_str(
            "lods = [{layout='POSITION', simplify=false}, {layout='TEXTURE0', simplify=false}]",
        )
        .unwrap();
        let resolved = mesh.resolve_lod_requests(defaults).unwrap();
        assert_eq!(resolved.len(), 3);
        assert_eq!(resolved[0].layout, VertexType::POSITION);
        assert!(!resolved[0].simplify);
        assert_eq!(resolved[1].layout, VertexType::TEXTURE0);
        assert!(!resolved[1].simplify);
        assert_eq!(resolved[2], defaults[1]);
        assert_eq!(mesh.lod_requests().len(), 2, "raw recipe must stay local");

        for requests in [
            "",
            "lods = []",
            "lods = [{layout='POSITION', simplify=false}]",
        ] {
            let mesh: MeshAsset =
                toml::from_str(&format!("inherit-lods = false\n{requests}")).unwrap();
            assert!(!mesh.inherit_lods());
            assert_eq!(
                &*mesh.resolve_lod_requests(defaults).unwrap(),
                mesh.lod_requests()
            );
        }

        for inherit in [false, true] {
            let mesh: MeshAsset = toml::from_str(&format!(
                "inherit-lods = {inherit}\nlods = [{{layout='POSITION', simplify=true}}, {{layout='POSITION', simplify=false}}]"
            ))
            .unwrap();
            assert!(mesh.resolve_lod_requests(defaults).is_err());
        }
        assert!(toml::from_str::<MeshAsset>("inherit-lods = 'false'").is_err());
    }

    #[test]
    fn native_lod_flags_default_off_and_old_recipe_flags_are_rejected() {
        #[derive(serde::Deserialize)]
        struct Recipe {
            mesh: MeshAsset,
        }
        let recipe: Recipe =
            toml::from_str(include_str!("../../tests/data/scene/mesh_01.toml")).unwrap();
        assert!(recipe.mesh.lod_requests().is_empty());
        let default: MeshAsset = toml::from_str("").unwrap();
        assert!(default.lod_requests().is_empty());
        let enabled: MeshAsset = toml::from_str(LOD_REQUESTS).unwrap();
        assert_eq!(enabled.lod_requests().len(), 4);
        for old in [
            "lod = true",
            "shadow = true",
            "lod = false",
            "shadow = false",
            "guide-lods = true",
            "shadow-lods = true",
        ] {
            assert!(toml::from_str::<MeshAsset>(old).is_err());
        }
    }

    #[test]
    fn triangle_strip_rejects_short_inputs_before_meshopt() {
        for count in 0..3 {
            let mut indices = (0..count).collect::<Vec<_>>();
            let original = indices.clone();
            let error =
                MeshAsset::convert_triangle_strip_to_list(&mut indices, u32::MAX).unwrap_err();
            assert!(error.to_string().contains("at least 3 indices"));
            assert_eq!(indices, original);
        }
        let mut indices = vec![2, 4, 6];
        MeshAsset::convert_triangle_strip_to_list(&mut indices, u32::MAX).unwrap();
        assert_eq!(indices, [2, 4, 6]);
    }

    #[test]
    fn clustered_lods_reduce_iteratively_with_accumulated_absolute_error() {
        let source = grid(25, true);
        for lock_border in [false, true] {
            let asset: MeshAsset = toml::from_str(&format!(
                "lod-target-error = 1.0\nmin-lod-triangles = 64\nlod-lock-border = {lock_border}"
            ))
            .unwrap();
            let lods = asset.calculate_lods(source.clone()).unwrap();
            assert!(lods.len() >= 3);
            assert_eq!(lods[0].error(), 0.0);
            let adapter =
                VertexDataAdapter::new(source.vertex_data(), source.vertex_type().stride(), 0)
                    .unwrap();
            let scale = simplify_scale(&adapter);
            let mut previous = source.indices().triangle_count();
            let mut previous_error = 0.0;
            assert!(lods[1].geometry().indices().triangle_count() > previous / 3);
            for lod in &lods[1..] {
                assert!(lod.error() >= previous_error);
                assert!(lod.error() <= scale.next_up());
                let count = lod.geometry().indices().triangle_count();
                assert!(count < previous && count >= 64);
                assert!(lod.error() > 0.0);
                previous = count;
                previous_error = lod.error();
            }
        }
    }

    #[test]
    fn clustered_guide_supersedes_ordinary_prefix_with_deterministic_immutable_vertices() {
        for lock_border in [false, true] {
            let asset: MeshAsset = toml::from_str(&format!(
                "min-lod-triangles = 8\nlod-lock-border = {lock_border}"
            ))
            .unwrap();
            let guide = asset
                .layout_lods(&grid(25, true), VertexType::PACKED_NORMAL)
                .unwrap();
            let source = guide.levels()[0].geometry();
            // The clustered producer intentionally supersedes the old byte-exact ordinary prefix.
            let enhanced = asset.calculate_lods(source.clone()).unwrap();
            assert!(enhanced.len() >= 3);
            for lod in &enhanced {
                for vertex in lod.geometry().vertex_data().chunks_exact(16) {
                    assert!(
                        source
                            .vertex_data()
                            .chunks_exact(16)
                            .any(|original| original == vertex)
                    );
                }
            }
            assert_eq!(enhanced, asset.calculate_lods(source.clone()).unwrap(),);
        }
    }

    #[test]
    fn packed_guide_reduces_hard_normal_splits_and_preserves_geometry_and_borders() {
        let mut data = VertexData {
            positions: vec![],
            normals: vec![],
            textures: (vec![], vec![]),
            indices: vec![],
            tangents: vec![],
            skin: None,
        };
        // Subdivide an octahedron into a closed unit sphere, then split every face normal.
        let mut faces = Vec::new();
        for x in [-1.0, 1.0] {
            for y in [-1.0, 1.0] {
                for z in [-1.0, 1.0] {
                    let mut face = [glam::Vec3::X * x, glam::Vec3::Y * y, glam::Vec3::Z * z];
                    if x * y * z < 0.0 {
                        face.swap(1, 2);
                    }
                    faces.push(face);
                }
            }
        }
        for _ in 0..3 {
            faces = faces
                .into_iter()
                .flat_map(|[a, b, c]| {
                    let ab = (a + b).normalize();
                    let bc = (b + c).normalize();
                    let ca = (c + a).normalize();
                    [[a, ab, ca], [ab, b, bc], [ca, bc, c], [ab, bc, ca]]
                })
                .collect();
        }
        for [a, b, c] in faces {
            let normal = (b - a).cross(c - a).normalize();
            for position in [a, b, c] {
                data.indices.push(data.positions.len() as u32);
                data.positions.push(position.to_array());
                data.normals.push(normal.to_array());
                data.textures.0.push([position.x, position.y]);
            }
        }
        let (vertex_type, vertices) = data.to_vertex_buf();
        let base = Geometry::new(
            &vertices,
            vertex_type,
            IndexBuffer::new(&data.indices).unwrap(),
        )
        .unwrap();
        for lock_border in [false, true] {
            let asset: MeshAsset = toml::from_str(&format!(
                "{LOD_REQUESTS}\nmin-lod-triangles = 8\nlod-target-error = 0.02\nlod-lock-border = {lock_border}"
            )).unwrap();
            let mut primitive = Primitive::new(0, base.clone()).unwrap();
            asset.generate_lods(&mut primitive).unwrap();
            assert_eq!(primitive.base(), &base);
            assert_full_detail_and_compact_lods(&primitive);
            let guide = primitive
                .lod_set(VertexType::PACKED_NORMAL)
                .unwrap()
                .levels();
            let source = guide[0].geometry();
            let adapter = VertexDataAdapter::new(source.vertex_data(), 16, 0).unwrap();
            let old = simplify(
                &source.indices().as_u32(),
                &adapter,
                24,
                asset.lod_target_error(),
                SimplifyOptions::None,
                None,
            );
            assert_eq!(
                old.len() / 3,
                512,
                "ordinary simplification stalls at hard seams"
            );
            let open = asset
                .layout_lods(&grid(5, false), VertexType::PACKED_NORMAL)
                .unwrap();
            assert!(open.levels().len() > 1);
            let border = (0..open.levels()[0].geometry().vertex_count() as u32)
                .map(|idx| open.levels()[0].geometry().position(idx))
                .filter(|p| p.x == 0.0 || p.x == 4.0 || p.y == 0.0 || p.y == 4.0)
                .collect::<Vec<_>>();
            for lod in &open.levels()[1..] {
                let retained = border.iter().all(|position| {
                    (0..lod.geometry().vertex_count() as u32)
                        .any(|idx| lod.geometry().position(idx) == *position)
                });
                assert_eq!(
                    retained, lock_border,
                    "only locked borders must retain every vertex"
                );
            }
            let reduced = guide.last().unwrap().geometry().indices().triangle_count();
            assert!(
                reduced <= 448,
                "hard-split sphere reduced to {reduced} triangles"
            );
            let repeated = asset.layout_lods(&base, VertexType::PACKED_NORMAL).unwrap();
            assert_eq!(
                primitive.lod_set(VertexType::PACKED_NORMAL),
                Some(&repeated)
            );
            for lod in &guide[1..] {
                assert!(lod.error().is_finite() && lod.error() > 0.0);
                assert!(
                    lod.error() <= (asset.lod_target_error() * simplify_scale(&adapter)).next_up()
                );
                let geometry = lod.geometry();
                for vertex in geometry.vertex_data().chunks_exact(16) {
                    assert!(
                        source
                            .vertex_data()
                            .chunks_exact(16)
                            .any(|original| original == vertex)
                    );
                }
                for triangle in geometry.indices().as_u32().chunks_exact(3) {
                    let positions = triangle
                        .iter()
                        .map(|&idx| geometry.position(idx))
                        .collect::<Vec<_>>();
                    let face = (positions[1] - positions[0])
                        .cross(positions[2] - positions[0])
                        .normalize();
                    for (corner, &idx) in triangle.iter().enumerate() {
                        let position = positions[corner];
                        let midpoint = (position + positions[(corner + 1) % 3]) * 0.5;
                        // The source tessellation itself deviates from the ideal sphere.
                        assert!(1.0 - midpoint.length() <= lod.error() as f64 + 0.02);
                        assert!(1.0 - face.dot(position) <= lod.error() as f64 + 0.02);
                        assert!((position.length() - 1.0).abs() < 1e-6);
                        let offset = idx as usize * 16;
                        let vertex = &geometry.vertex_data()[offset..offset + 16];
                        let x =
                            i16::from_ne_bytes(vertex[12..14].try_into().unwrap()) as f64 / 32767.0;
                        let y =
                            i16::from_ne_bytes(vertex[14..16].try_into().unwrap()) as f64 / 32767.0;
                        let mut normal = glam::DVec3::new(x, y, 1.0 - x.abs() - y.abs());
                        let t = (-normal.z).max(0.0);
                        normal.x += if x >= 0.0 { -t } else { t };
                        normal.y += if y >= 0.0 { -t } else { t };
                        assert!(
                            face.dot(normal.normalize()) > 0.95,
                            "faceted shading must follow the surface: {}",
                            face.dot(normal.normalize())
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn lod_minimum_and_no_progress_terminate_independently_of_error() {
        let floor: MeshAsset = toml::from_str("min-lod-triangles = 1000").unwrap();
        assert_eq!(floor.calculate_lods(grid(3, false)).unwrap().len(), 1);
        for error in [0.0, 1.0] {
            let locked: MeshAsset = toml::from_str(&format!(
                "min-lod-triangles = 1\nlod-lock-border = true\nlod-target-error = {error}"
            ))
            .unwrap();
            assert_eq!(locked.calculate_lods(grid(2, false)).unwrap().len(), 1);
        }
        let floor: MeshAsset =
            toml::from_str("min-lod-triangles = 65\nlod-target-error = 1.0").unwrap();
        let lods = floor.calculate_lods(grid(17, false)).unwrap();
        assert!(lods.len() > 1);
        assert!(
            lods.iter()
                .all(|lod| lod.geometry().indices().triangle_count() >= 65)
        );
        for error in ["nan", "inf", "-1.0"] {
            let invalid: MeshAsset =
                toml::from_str(&format!("lod-target-error = {error}")).unwrap();
            assert!(invalid.calculate_lods(grid(2, false)).is_err());
        }
    }

    #[test]
    fn accumulated_lod_error_rounds_outward_and_rejects_invalid_metrics() {
        for (error, scale) in [(0.0, 7.0), (0.1, 3.1), (f32::from_bits(1), 0.5), (1.0, 0.0)] {
            let absolute = super::lod::rounded_error(error as f64 * scale as f64).unwrap();
            assert!(absolute.is_finite());
            assert!(absolute as f64 >= error as f64 * scale as f64);
            assert_eq!(absolute == 0.0, error == 0.0 || scale == 0.0);
        }
        for (error, scale) in [
            (f32::NAN, 1.0),
            (1.0, f32::INFINITY),
            (-1.0, 1.0),
            (f32::MAX, 2.0),
        ] {
            assert!(super::lod::rounded_error(error as f64 * scale as f64).is_err());
        }
    }

    #[test]
    fn purpose_welding_uses_final_canonical_pairs_and_preserves_opacity_seams() {
        let asset: MeshAsset =
            toml::from_str(&format!("{LOD_REQUESTS}\nmin-lod-triangles = 1")).unwrap();
        let data = VertexData {
            positions: vec![
                [0.0, 0.0, 0.0],
                [1.0, 0.0, 0.0],
                [0.0, 1.0, 0.0],
                [1.0, 0.0, 0.0],
                [1.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            normals: vec![
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [0.0, 0.0, 1.0],
                [0.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
                [0.0, 1.0, 0.0],
            ],
            textures: (
                vec![
                    [0.0, 0.0],
                    [1.0, 0.0],
                    [0.0, 1.0],
                    [0.25, 0.0],
                    [1.0, 1.0],
                    [0.0, 0.75],
                ],
                vec![],
            ),
            indices: vec![4, 5, 3, 2, 0, 1],
            tangents: vec![],
            skin: None,
        };
        let (vertex_type, mut vertices) = data.to_vertex_buf();
        let original = Geometry::new(
            &vertices,
            vertex_type,
            IndexBuffer::new(&data.indices).unwrap(),
        )
        .unwrap();
        let mut indices = data.indices.clone();
        asset
            .optimize_mesh(&mut indices, &mut vertices, vertex_type.stride())
            .unwrap();
        let base =
            Geometry::new(&vertices, vertex_type, IndexBuffer::new(&indices).unwrap()).unwrap();
        assert_ne!(base.vertex_data(), original.vertex_data());
        assert_eq!(triangles(&base, true), triangles(&original, true));
        let mut primitive = Primitive::new(7, base.clone()).unwrap();
        asset.generate_lods(&mut primitive).unwrap();
        assert_eq!(primitive.base(), &base);
        assert_eq!(primitive.material(), 7);
        assert_full_detail_and_compact_lods(&primitive);
        assert_eq!(
            primitive
                .lod_set(VertexType::PACKED_NORMAL)
                .unwrap()
                .levels()[0]
                .geometry()
                .vertex_count(),
            6
        );
        assert_eq!(
            primitive.lod_set(VertexType::POSITION).unwrap().levels()[0]
                .geometry()
                .vertex_count(),
            6
        );
        for opaque_type in [VertexType::PACKED_NORMAL, VertexType::POSITION] {
            let set = primitive.lod_set(opaque_type).unwrap();
            let full = &set.levels()[0];
            assert_eq!(triangles(full.geometry(), false), triangles(&base, false));
            assert_eq!(full.geometry().vertex_type(), opaque_type);
            assert_eq!(full.error(), 0.0);
            let exact = &primitive
                .lod_set(opaque_type | VertexType::TEXTURE0)
                .unwrap()
                .levels()[0];
            assert_eq!(
                exact.geometry().vertex_type(),
                opaque_type | VertexType::TEXTURE0
            );
            assert_eq!(exact.geometry().vertex_count(), 6);
            assert_eq!(triangles(exact.geometry(), true), triangles(&base, true));
            assert!(!exact.patches().is_empty());
            for lod in set.levels() {
                assert!(!lod.patches().is_empty());
            }
        }
        let encoded = bincode::serde::encode_to_vec(&primitive, bincode::config::legacy()).unwrap();
        let (decoded, _): (Primitive, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::legacy()).unwrap();
        assert_eq!(decoded, primitive);
    }

    #[test]
    fn packed_guide_normals_decode_as_octahedral_snorm2x16() {
        let asset: MeshAsset =
            toml::from_str("lods = [{layout='PACKED_NORMAL', simplify=true}]").unwrap();
        let mut data = VertexData {
            positions: vec![],
            normals: vec![],
            textures: (vec![], vec![]),
            indices: (0..9).collect(),
            tangents: vec![],
            skin: None,
        };
        for (index, normal) in [
            glam::Vec3::X,
            glam::Vec3::NEG_X,
            glam::Vec3::Y,
            glam::Vec3::NEG_Y,
            glam::Vec3::Z,
            glam::Vec3::NEG_Z,
            glam::vec3(1.0, -2.0, 3.0),
            glam::vec3(-1.0, 2.0, -3.0),
            glam::Vec3::ONE,
        ]
        .into_iter()
        .enumerate()
        {
            data.positions.push([index as f32, (index % 3) as f32, 0.0]);
            data.normals.push(normal.normalize().to_array());
        }
        let (vertex_type, vertices) = data.to_vertex_buf();
        let mut primitive = Primitive::new(
            0,
            Geometry::new(
                &vertices,
                vertex_type,
                IndexBuffer::new(&data.indices).unwrap(),
            )
            .unwrap(),
        )
        .unwrap();
        asset.generate_lods(&mut primitive).unwrap();
        let guide = primitive
            .lod_set(VertexType::PACKED_NORMAL)
            .unwrap()
            .levels()[0]
            .geometry();
        assert_eq!(guide.vertex_type().stride(), 16);
        assert!(
            primitive
                .lod_set(VertexType::PACKED_NORMAL | VertexType::TEXTURE0)
                .is_none()
        );
        for vertex in guide.vertex_data().chunks_exact(16) {
            let index = f32::from_ne_bytes(vertex[..4].try_into().unwrap()) as usize;
            let x = i16::from_ne_bytes(vertex[12..14].try_into().unwrap()) as f64 / 32767.0;
            let y = i16::from_ne_bytes(vertex[14..16].try_into().unwrap()) as f64 / 32767.0;
            let mut normal = glam::DVec3::new(x, y, 1.0 - x.abs() - y.abs());
            let t = (-normal.z).max(0.0);
            normal.x += if x >= 0.0 { -t } else { t };
            normal.y += if y >= 0.0 { -t } else { t };
            assert!(
                normal
                    .normalize()
                    .dot(glam::DVec3::from_array(data.normals[index].map(f64::from)).normalize())
                    > 0.99999999
            );
        }
    }

    #[test]
    fn missing_requested_attributes_fail_without_mutating_source_or_inventing_fallbacks() {
        let asset: MeshAsset = toml::from_str(LOD_REQUESTS).unwrap();
        let base = Geometry::new(
            &[0; 60],
            VertexType::JOINTS_WEIGHTS,
            IndexBuffer::new(&[0, 1, 2]).unwrap(),
        )
        .unwrap();
        let mut primitive = Primitive::new(2, base.clone()).unwrap();
        assert!(asset.generate_lods(&mut primitive).is_err());
        assert_eq!(primitive.base(), &base);
        assert!(primitive.lod_sets().is_empty());
        let base = Geometry::new(
            &[0; 36],
            VertexType::POSITION,
            IndexBuffer::new(&[0, 1, 2]).unwrap(),
        )
        .unwrap();
        let mut primitive = Primitive::new(2, base.clone()).unwrap();
        assert!(asset.generate_lods(&mut primitive).is_err());
        assert_eq!(primitive.base(), &base);
        assert!(primitive.lod_sets().is_empty());

        let source = grid(2, false);
        let mut vertices = source.vertex_data().to_vec();
        vertices[24..28].copy_from_slice(&f32::NAN.to_ne_bytes());
        let base =
            Geometry::new(&vertices, source.vertex_type(), source.indices().clone()).unwrap();
        let mut primitive = Primitive::new(2, base.clone()).unwrap();
        asset.generate_lods(&mut primitive).unwrap();
        assert_eq!(primitive.base(), &base);
        assert!(
            primitive
                .lod_set(VertexType::PACKED_NORMAL | VertexType::TEXTURE0)
                .unwrap()
                .levels()[0]
                .geometry()
                .texture0()
                .unwrap()
                .any(|uv| uv.into_iter().any(f32::is_nan))
        );
        vertices[12..16].copy_from_slice(&f32::NAN.to_ne_bytes());
        let base =
            Geometry::new(&vertices, source.vertex_type(), source.indices().clone()).unwrap();
        let mut primitive = Primitive::new(2, base.clone()).unwrap();
        assert!(asset.generate_lods(&mut primitive).is_err());
        assert_eq!(primitive.base(), &base);
        assert!(primitive.lod_sets().is_empty());
    }

    #[test]
    fn enabling_native_alternatives_preserves_imported_ordered_base_bytes_and_materials() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/scene/cube.glb");
        for optimization in [
            "",
            "optimize = false",
            "optimize = false\noptimize-vertex-cache = true",
        ] {
            let disabled: MeshAsset = toml::from_str(optimization).unwrap();
            let enabled: MeshAsset = toml::from_str(&format!(
                "{optimization}\n{LOD_REQUESTS}\nmin-lod-triangles = 1"
            ))
            .unwrap();
            let base = disabled.to_mesh(&src).unwrap();
            let alternatives = enabled.to_mesh(&src).unwrap();
            assert_eq!(base.primitives().len(), alternatives.primitives().len());
            for (base, alternative) in base.primitives().iter().zip(alternatives.primitives()) {
                assert_eq!(base.base(), alternative.base());
                assert_eq!(base.material(), alternative.material());
                assert_full_detail_and_compact_lods(alternative);
            }
        }
    }

    #[test]
    fn alternatives_use_the_actually_reduced_max_index_canonical_pair() {
        let source = grid(17, true);
        for optimization in [
            "",
            "optimize = false",
            "optimize = false\noptimize-vertex-cache = true",
        ] {
            let mut expected = None;
            for enabled in [false, true] {
                let requests = if enabled { LOD_REQUESTS } else { "lods = []" };
                let asset: MeshAsset = toml::from_str(&format!("{optimization}\nmax-index = 127\nlod-target-error = 1.0\nmin-lod-triangles = 8\n{requests}")).unwrap();
                let mut indices = source.indices().as_u32();
                let mut vertices = source.vertex_data().to_vec();
                let stride = source.vertex_type().stride();
                asset
                    .optimize_mesh(&mut indices, &mut vertices, stride)
                    .unwrap();
                assert!(indices.iter().any(|&index| index > 127));
                asset
                    .reduce_mesh_to_max_index(&mut indices, &mut vertices, stride)
                    .unwrap();
                assert!(indices.len() < source.indices().index_count());
                assert!(indices.iter().all(|&index| index <= 127));
                let base = Geometry::new(
                    &vertices,
                    source.vertex_type(),
                    IndexBuffer::new(&indices).unwrap(),
                )
                .unwrap();
                assert_compact(&base);
                let mut primitive = Primitive::new(9, base.clone()).unwrap();
                asset.generate_lods(&mut primitive).unwrap();
                assert_eq!(primitive.base(), &base);
                assert_eq!(primitive.material(), 9);
                if let Some(expected) = expected {
                    assert_eq!(primitive.base(), &expected);
                }
                expected = Some(base);
                if enabled {
                    assert!(
                        primitive
                            .lod_set(VertexType::PACKED_NORMAL)
                            .unwrap()
                            .levels()
                            .len()
                            > 1
                    );
                    assert!(
                        primitive
                            .lod_set(VertexType::POSITION)
                            .unwrap()
                            .levels()
                            .len()
                            > 1
                    );
                    assert_full_detail_and_compact_lods(&primitive);
                } else {
                    assert!(primitive.lod_sets().is_empty());
                }
            }
        }
    }

    #[test]
    fn imported_source_deformation_is_a_fact_not_a_lod_storage_veto() {
        let directory = tempfile::tempdir().unwrap();
        let source = grid(2, false);
        let mut data = source
            .vertex_data()
            .chunks_exact(4)
            .flat_map(|value| f32::from_ne_bytes(value.try_into().unwrap()).to_le_bytes())
            .collect::<Vec<_>>();
        data.extend(
            source
                .indices()
                .as_u32()
                .into_iter()
                .flat_map(u32::to_le_bytes),
        );
        data.extend([0; 16]); // four sets of u8 joint indices
        for _ in 0..4 {
            data.extend(
                [1.0_f32, 0.0, 0.0, 0.0]
                    .into_iter()
                    .flat_map(f32::to_le_bytes),
            );
        }
        data.extend(
            glam::Mat4::IDENTITY
                .to_cols_array()
                .into_iter()
                .flat_map(f32::to_le_bytes),
        );
        assert_eq!(data.len(), 296);
        fs::write(directory.path().join("geometry.bin"), data).unwrap();
        let path = directory.path().join("deformation.gltf");
        fs::write(&path, r#"{
            "asset": {"version": "2.0"},
            "buffers": [{"uri": "geometry.bin", "byteLength": 296}],
            "bufferViews": [
                {"buffer": 0, "byteOffset": 0, "byteLength": 128, "byteStride": 32, "target": 34962},
                {"buffer": 0, "byteOffset": 128, "byteLength": 24, "target": 34963},
                {"buffer": 0, "byteOffset": 152, "byteLength": 16, "target": 34962},
                {"buffer": 0, "byteOffset": 168, "byteLength": 64, "target": 34962},
                {"buffer": 0, "byteOffset": 232, "byteLength": 64}
            ],
            "accessors": [
                {"bufferView": 0, "byteOffset": 0, "componentType": 5126, "count": 4, "type": "VEC3", "min": [0, 0, 0], "max": [1, 1, 0]},
                {"bufferView": 0, "byteOffset": 12, "componentType": 5126, "count": 4, "type": "VEC3"},
                {"bufferView": 0, "byteOffset": 24, "componentType": 5126, "count": 4, "type": "VEC2"},
                {"bufferView": 1, "componentType": 5125, "count": 6, "type": "SCALAR"},
                {"bufferView": 2, "componentType": 5121, "count": 4, "type": "VEC4"},
                {"bufferView": 3, "componentType": 5126, "count": 4, "type": "VEC4"},
                {"bufferView": 4, "componentType": 5126, "count": 1, "type": "MAT4"}
            ],
            "meshes": [
                {"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}, "indices": 3}]},
                {"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2, "JOINTS_0": 4, "WEIGHTS_0": 5}, "indices": 3}]},
                {"primitives": [{"attributes": {"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}, "indices": 3, "targets": [{"POSITION": 0}]}]}
            ],
            "nodes": [
                {"name": "static", "mesh": 0},
                {"name": "skinned", "mesh": 1, "skin": 0},
                {"name": "morph", "mesh": 2},
                {"name": "root"}
            ],
            "skins": [{"joints": [3], "inverseBindMatrices": 6}],
            "scenes": [{"nodes": [0, 1, 2, 3]}],
            "scene": 0
        }"#).unwrap();
        for name in ["static", "skinned", "morph"] {
            for ignore_skin in [false, true] {
                let disabled: MeshAsset = toml::from_str(&format!(
                    "name = '{name}'\nignore-skin = {ignore_skin}\ntangents = false"
                ))
                .unwrap();
                let base = disabled.to_mesh(&path).unwrap();
                let mut enabled = disabled.clone();
                enabled.lods = Some(
                    toml::from_str::<MeshAsset>(LOD_REQUESTS)
                        .unwrap()
                        .lods
                        .unwrap(),
                );
                let mesh = enabled.to_mesh(&path).unwrap();
                assert_eq!(mesh.skin().is_some(), name == "skinned" && !ignore_skin);
                assert_eq!(mesh.primitives().len(), 1);
                let primitive = &mesh.primitives()[0];
                assert_eq!(primitive.base(), base.primitives()[0].base());
                assert_eq!(primitive.material(), base.primitives()[0].material());
                assert_eq!(
                    primitive
                        .base()
                        .vertex_type()
                        .contains(VertexType::JOINTS_WEIGHTS),
                    name == "skinned" && !ignore_skin
                );
                assert_eq!(primitive.lod_sets().len(), 4);
                assert_eq!(
                    mesh.data("pak.source-deformation")
                        .unwrap()
                        .as_iter()
                        .unwrap()
                        .next()
                        .unwrap()
                        .as_bool(),
                    Some(name != "static")
                );
                if name == "static" {
                    assert_full_detail_and_compact_lods(primitive);
                }
            }
        }
    }

    #[test]
    fn mesh_ffi_input_validation_rejects_malformed_pairs() {
        let asset = MeshAsset::new("unused.glb");
        for (mut vertices, stride, mut indices) in [
            (vec![0; 12], 12, vec![0, 1, 2]),
            (vec![0; 36], 12, vec![0, 1]),
            (vec![0; 13], 12, vec![0, 0, 0]),
            (vec![0; 12], 0, vec![0, 0, 0]),
            (f32::NAN.to_ne_bytes().repeat(3), 12, vec![0, 0, 0]),
        ] {
            assert!(
                asset
                    .optimize_mesh(&mut indices, &mut vertices, stride)
                    .is_err()
            );
            assert!(
                asset
                    .reduce_mesh_to_max_index(&mut indices, &mut vertices, stride)
                    .is_err()
            );
        }
    }

    #[test]
    fn imported_material_seams_survive_every_guide_and_shadow_level() {
        let directory = tempfile::tempdir().unwrap();
        let source = grid(17, true);
        let mut data = source
            .vertex_data()
            .chunks_exact(4)
            .flat_map(|v| f32::from_ne_bytes(v.try_into().unwrap()).to_le_bytes())
            .collect::<Vec<_>>();
        let vertex_bytes = data.len();
        let indices = source.indices().as_u32();
        let left = indices
            .chunks_exact(3)
            .filter(|t| t.iter().all(|&idx| source.position(idx).x <= 8.0))
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        let right = indices
            .chunks_exact(3)
            .filter(|t| t.iter().any(|&idx| source.position(idx).x > 8.0))
            .flatten()
            .copied()
            .collect::<Vec<_>>();
        data.extend(left.iter().chain(&right).flat_map(|idx| idx.to_le_bytes()));
        std::fs::write(directory.path().join("geometry.bin"), &data).unwrap();
        let path = directory.path().join("materials.gltf");
        std::fs::write(&path, format!(r#"{{
            "asset": {{"version": "2.0"}},
            "buffers": [{{"uri": "geometry.bin", "byteLength": {total}}}],
            "bufferViews": [
                {{"buffer": 0, "byteLength": {vertex_bytes}, "byteStride": 32}},
                {{"buffer": 0, "byteOffset": {vertex_bytes}, "byteLength": {index_bytes}}}
            ],
            "accessors": [
                {{"bufferView": 0, "componentType": 5126, "count": 289, "type": "VEC3", "min": [0,0,0], "max": [16,16,11]}},
                {{"bufferView": 0, "byteOffset": 12, "componentType": 5126, "count": 289, "type": "VEC3"}},
                {{"bufferView": 0, "byteOffset": 24, "componentType": 5126, "count": 289, "type": "VEC2"}},
                {{"bufferView": 1, "componentType": 5125, "count": {left_count}, "type": "SCALAR"}},
                {{"bufferView": 1, "byteOffset": {left_bytes}, "componentType": 5125, "count": {right_count}, "type": "SCALAR"}}
            ],
            "materials": [{{}}, {{}}],
            "meshes": [{{"primitives": [
                {{"attributes": {{"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}}, "indices": 3, "material": 0}},
                {{"attributes": {{"POSITION": 0, "NORMAL": 1, "TEXCOORD_0": 2}}, "indices": 4, "material": 1}}
            ]}}],
            "nodes": [{{"mesh": 0}}], "scenes": [{{"nodes": [0]}}], "scene": 0
        }}"#, total = data.len(), index_bytes = (left.len() + right.len()) * 4, left_count = left.len(), left_bytes = left.len() * 4, right_count = right.len())).unwrap();
        let asset: MeshAsset = toml::from_str(&format!("{LOD_REQUESTS}\ntangents = false\nmin-lod-triangles = 8\nlod-target-error = 0.1\nlod-lock-border = false")).unwrap();
        let mesh = asset.to_mesh(path).unwrap();
        assert_eq!(mesh.primitives().len(), 2);
        for (idx, primitive) in mesh.primitives().iter().enumerate() {
            assert_eq!(primitive.material(), idx as u8);
            assert_full_detail_and_compact_lods(primitive);
            for set in primitive
                .lod_sets()
                .iter()
                .filter(|set| !set.vertex_type().contains(VertexType::TEXTURE0))
            {
                assert!(set.levels().len() > 1);
                for lod in set.levels() {
                    let seam = (0..lod.geometry().vertex_count() as u32)
                        .map(|idx| lod.geometry().position(idx))
                        .filter(|p| p.x == 8.0)
                        .collect::<Vec<_>>();
                    assert_eq!(
                        seam.len(),
                        17,
                        "shared material border must retain every position"
                    );
                }
            }
        }
    }

    #[test]
    fn max_index_deserializes_enum_variants() {
        let mesh: MeshAsset =
            toml::from_str("max-index = 'u8'").expect("max-index should accept u8 variant");
        assert_eq!(mesh.max_index, Some(MaxIndex::U8));
        assert_eq!(mesh.max_index(), Some(u8::MAX as u32));

        let mesh: MeshAsset = toml::from_str("max-index = 'U16'")
            .expect("max-index should accept u16 variant case-insensitively");
        assert_eq!(mesh.max_index, Some(MaxIndex::U16));
        assert_eq!(mesh.max_index(), Some(u16::MAX as u32));
    }

    #[test]
    fn max_index_deserializes_exact_value() {
        let mesh: MeshAsset =
            toml::from_str("max-index = 4095").expect("max-index should accept exact values");
        assert_eq!(mesh.max_index, Some(MaxIndex::Exact(4095)));
        assert_eq!(mesh.max_index(), Some(4095));
    }

    #[test]
    fn ignore_texture1_defaults_to_false_and_deserializes() {
        let default: MeshAsset = toml::from_str("").expect("default mesh should deserialize");
        assert!(!default.ignore_texture1());

        let ignored: MeshAsset =
            toml::from_str("ignore-texture1 = true").expect("ignore-texture1 should deserialize");
        assert!(ignored.ignore_texture1());
    }

    #[test]
    fn compact_mesh_indices_rewrites_sparse_indices() {
        let vertex_stride = 4;
        let vertex_buf = [10, 0, 0, 0, 20, 0, 0, 0, 30, 0, 0, 0, 40, 0, 0, 0];
        let mut indices = vec![3, 1, 3, 2, 1, 3];

        let (compact, vertex_count) =
            MeshAsset::compact_mesh_indices(&mut indices, &vertex_buf, vertex_stride)
                .expect("mesh should compact");

        assert_eq!(indices, [0, 1, 0, 2, 1, 0]);
        assert_eq!(vertex_count, 3);
        assert_eq!(compact, [40, 0, 0, 0, 20, 0, 0, 0, 30, 0, 0, 0]);
    }

    #[test]
    fn cache_only_optimization_preserves_vertices_and_oriented_triangles() {
        let disabled: MeshAsset = toml::from_str("optimize = false").unwrap();
        let cache_only: MeshAsset =
            toml::from_str("optimize = false\noptimize-vertex-cache = true").unwrap();
        assert!(!disabled.optimize_vertex_cache());
        assert!(!cache_only.optimize());
        assert!(MeshAsset::new("mesh.glb").optimize_vertex_cache());
        let mut vertices = Vec::new();
        for y in 0..24 {
            for x in 0..24 {
                for value in [
                    x as f32,
                    y as f32,
                    0.0,
                    (x + y) as f32,
                    x as f32 / 23.0,
                    y as f32 / 23.0,
                ] {
                    vertices.extend_from_slice(&value.to_ne_bytes());
                }
            }
        }
        let mut triangles = Vec::new();
        for y in 0..23 {
            for x in 0..23 {
                let i = y * 24 + x;
                triangles.extend([[i, i + 1, i + 24], [i + 1, i + 25, i + 24]]);
            }
        }
        let count = triangles.len();
        triangles.sort_by_key(|triangle| {
            ((triangle[0] * 2 + u32::from(triangle[1] == triangle[0] + 24)) as usize * 97) % count
        });
        let mut indices = triangles.into_iter().flatten().collect::<Vec<_>>();
        let mut optimized_indices = indices.clone();
        let mut optimized_vertices = vertices.clone();
        disabled
            .optimize_mesh(&mut indices, &mut vertices, 24)
            .unwrap();
        cache_only
            .optimize_mesh(&mut optimized_indices, &mut optimized_vertices, 24)
            .unwrap();
        assert_eq!(vertices, optimized_vertices);
        let oriented = |indices: &[u32]| {
            let mut triangles = indices
                .chunks_exact(3)
                .map(|triangle| <[u32; 3]>::try_from(triangle).unwrap())
                .collect::<Vec<_>>();
            triangles.sort();
            triangles
        };
        assert_eq!(oriented(&indices), oriented(&optimized_indices));
        let before = meshopt::analyze_vertex_cache(&indices, 24 * 24, 32, 32, 256);
        let after = meshopt::analyze_vertex_cache(&optimized_indices, 24 * 24, 32, 32, 256);
        assert!(after.vertices_transformed < before.vertices_transformed);
    }

    #[test]
    fn mesh_scale_applies_to_imported_positions() {
        let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/data/scene/cube.glb");
        let unscaled: MeshAsset = toml::from_str(
            "
            optimize = false
            normals = false
            tangents = false
            ",
        )
        .expect("unscaled mesh config should deserialize");
        let scaled: MeshAsset = toml::from_str(
            "
            scale = 2.0
            optimize = false
            normals = false
            tangents = false
            ",
        )
        .expect("scaled mesh config should deserialize");

        let unscaled = unscaled.to_mesh(&src).expect("unscaled mesh should import");
        let scaled = scaled.to_mesh(&src).expect("scaled mesh should import");
        let unscaled_max = max_abs_position(&unscaled);
        let scaled_max = max_abs_position(&scaled);

        assert!(unscaled_max > 0.0);
        assert!((scaled_max - unscaled_max * 2.0).abs() < 0.0001);
    }
}
