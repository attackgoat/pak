use {
    super::{Mat4, index::IndexBuffer},
    crate::{
        BlobId,
        scene::{DataMap, DataRef},
    },
    anyhow::{Context, ensure},
    bitflags::bitflags,
    glam::DVec3,
    meshopt::{VertexDataAdapter, build_meshlets},
    serde::{Deserialize, Deserializer, Serialize, de::Error as _},
    std::ops::Range,
};

/// Mesh-local axis-aligned bounds of the actual referenced geometry, not an error bound.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct Bounds {
    min: [f32; 3],
    max: [f32; 3],
}

impl Bounds {
    pub fn min(&self) -> [f32; 3] {
        self.min
    }

    pub fn max(&self) -> [f32; 3] {
        self.max
    }
}

/// A cone enclosing geometric face normals. Normalize `axis` before use.
/// `min_dot` is a conservative lower bound on dot(axis, face_normal).
/// Perspective rejection must also account for the view directions over the patch bounds.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Serialize)]
pub struct FaceCone {
    axis: [f32; 3],
    min_dot: f32,
}

impl FaceCone {
    pub fn axis(&self) -> [f32; 3] {
        self.axis
    }

    pub fn min_dot(&self) -> f32 {
        self.min_dot
    }
}

/// One validated, interleaved vertex buffer and its matching triangle index buffer.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Geometry {
    indices: IndexBuffer,

    #[serde(with = "serde_bytes")]
    vertex_buf: Vec<u8>,

    vertex_type: VertexType,
}

impl Geometry {
    pub fn new(
        vertex_buf: &[u8],
        vertex_type: VertexType,
        indices: IndexBuffer,
    ) -> anyhow::Result<Self> {
        vertex_type.validate()?;
        Self::validate_pair(vertex_buf, vertex_type.stride(), &indices.as_u32())?;
        Ok(Self {
            indices,
            vertex_buf: vertex_buf.to_vec(),
            vertex_type,
        })
    }

    pub fn indices(&self) -> &IndexBuffer {
        &self.indices
    }

    pub(crate) fn position(&self, index: u32) -> DVec3 {
        let offset = index as usize * self.vertex_type.stride();
        DVec3::from_array(std::array::from_fn(|axis| {
            let offset = offset + axis * 4;
            f32::from_ne_bytes(self.vertex_buf[offset..offset + 4].try_into().unwrap()) as f64
        }))
    }

    /// Returns the first texture coordinate for each vertex, when present.
    pub fn texture0(&self) -> Option<impl ExactSizeIterator<Item = [f32; 2]> + '_> {
        if !self.vertex_type.contains(VertexType::TEXTURE0) {
            return None;
        }

        let offset = 12
            + if self.vertex_type.contains(VertexType::NORMAL) {
                12
            } else if self.vertex_type.contains(VertexType::PACKED_NORMAL) {
                4
            } else {
                0
            };
        Some(
            self.vertex_buf
                .chunks_exact(self.vertex_type.stride())
                .map(move |vertex| {
                    std::array::from_fn(|axis| {
                        let offset = offset + axis * 4;
                        f32::from_ne_bytes(vertex[offset..offset + 4].try_into().unwrap())
                    })
                }),
        )
    }

    // Also used before meshoptimizer FFI, before a typed Geometry has been finalized.
    pub(crate) fn validate_pair(
        vertex_buf: &[u8],
        stride: usize,
        indices: &[u32],
    ) -> anyhow::Result<()> {
        ensure!(
            stride >= 12 && stride <= 256 && stride.is_multiple_of(4),
            "invalid geometry vertex stride"
        );
        ensure!(
            !vertex_buf.is_empty() && vertex_buf.len().is_multiple_of(stride),
            "malformed geometry vertex buffer"
        );
        let vertex_count = vertex_buf.len() / stride;
        ensure!(
            vertex_count <= u32::MAX as usize,
            "geometry has too many vertices"
        );
        ensure!(
            indices.len() >= 3 && indices.len().is_multiple_of(3),
            "geometry indices must be triangles"
        );
        ensure!(
            indices.len() <= u32::MAX as usize,
            "geometry has too many indices"
        );
        ensure!(
            indices.iter().all(|&index| (index as usize) < vertex_count),
            "geometry index exceeds vertex count"
        );
        for vertex in vertex_buf.chunks_exact(stride) {
            for value in vertex[..12].chunks_exact(4) {
                ensure!(
                    f32::from_ne_bytes(value.try_into().unwrap()).is_finite(),
                    "geometry position must be finite"
                );
            }
        }
        Ok(())
    }

    pub fn vertex_count(&self) -> usize {
        self.vertex_buf.len() / self.vertex_type.stride()
    }

    pub fn vertex_data(&self) -> &[u8] {
        &self.vertex_buf
    }

    pub fn vertex_type(&self) -> VertexType {
        self.vertex_type
    }

    /// Projects attributes without changing vertex identity or triangle ordering.
    /// Float normals may be encoded as packed octahedral normals when requested.
    pub fn project(&self, layout: VertexType) -> anyhow::Result<Self> {
        layout.validate()?;
        let attributes = [
            (VertexType::NORMAL, 12),
            (VertexType::PACKED_NORMAL, 4),
            (VertexType::TEXTURE0, 8),
            (VertexType::TEXTURE1, 8),
            (VertexType::TANGENT, 16),
            (VertexType::JOINTS_WEIGHTS, 8),
        ];
        let mut source_offsets = [None; 6];
        let mut offset = 12;
        for (idx, &(attribute, bytes)) in attributes.iter().enumerate() {
            if self.vertex_type.contains(attribute) {
                source_offsets[idx] = Some(offset);
                offset += bytes;
            }
        }
        let mut output = Vec::with_capacity(self.vertex_count() * layout.stride());
        for vertex in self.vertex_data().chunks_exact(self.vertex_type.stride()) {
            output.extend_from_slice(&vertex[..12]);
            for (idx, &(attribute, bytes)) in attributes.iter().enumerate() {
                if !layout.contains(attribute) {
                    continue;
                }
                if let Some(offset) = source_offsets[idx] {
                    output.extend_from_slice(&vertex[offset..offset + bytes]);
                } else if attribute == VertexType::PACKED_NORMAL {
                    let offset =
                        source_offsets[0].context("packed normal projection requires normals")?;
                    let normal = DVec3::from_array(std::array::from_fn(|axis| {
                        f32::from_ne_bytes(
                            vertex[offset + axis * 4..offset + axis * 4 + 4]
                                .try_into()
                                .unwrap(),
                        ) as f64
                    }));
                    ensure!(
                        normal.is_finite() && normal.length_squared() > 0.0,
                        "invalid normal for packed projection"
                    );
                    let normal = normal / normal.abs().element_sum();
                    let oct = if normal.z < 0.0 {
                        [
                            (1.0 - normal.y.abs()) * if normal.x < 0.0 { -1.0 } else { 1.0 },
                            (1.0 - normal.x.abs()) * if normal.y < 0.0 { -1.0 } else { 1.0 },
                        ]
                    } else {
                        [normal.x, normal.y]
                    };
                    for value in oct {
                        output.extend_from_slice(&((value * 32767.0).round() as i16).to_ne_bytes());
                    }
                } else {
                    anyhow::bail!("requested vertex attribute {attribute:?} is absent");
                }
            }
        }
        Self::new(&output, layout, self.indices.clone())
    }
}

impl<'de> Deserialize<'de> for Geometry {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct GeometryData {
            indices: IndexBuffer,
            #[serde(with = "serde_bytes")]
            vertex_buf: Vec<u8>,
            vertex_type: VertexType,
        }
        let data = GeometryData::deserialize(deserializer)?;
        data.vertex_type.validate().map_err(D::Error::custom)?;
        Self::validate_pair(
            &data.vertex_buf,
            data.vertex_type.stride(),
            &data.indices.as_u32(),
        )
        .map_err(D::Error::custom)?;
        Ok(Self {
            indices: data.indices,
            vertex_buf: data.vertex_buf,
            vertex_type: data.vertex_type,
        })
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Joint {
    /// A matrix which transform the mesh into the local space of the joint.
    pub inverse_bind: Mat4,
    /// Name of the joint/bone.
    pub name: String,
    /// Index into the skin joints to the parent of this joint.
    pub parent_index: usize,
}

/// A completed alternative. Patch indices address only this LOD's geometry.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Lod {
    geometry: Geometry,
    error: f32,
    patches: Box<[TrianglePatch]>,
}

impl Lod {
    /// Partitions a completed static alternative without duplicating vertices.
    /// The error is the producing simplifier's absolute mesh-local metric, not a
    /// certified surface/coverage bound or a prefix-max selection envelope.
    pub fn new(mut geometry: Geometry, error: f32) -> anyhow::Result<Self> {
        ensure!(
            error.is_finite() && error >= 0.0,
            "lod error must be finite and nonnegative"
        );
        let indices = geometry.indices.as_u32();
        Geometry::validate_pair(
            geometry.vertex_data(),
            geometry.vertex_type.stride(),
            &indices,
        )?;
        let vertices =
            VertexDataAdapter::new(geometry.vertex_data(), geometry.vertex_type.stride(), 0)
                .context("creating patch vertex adapter")?;
        let meshlets = build_meshlets(&indices, &vertices, 64, 128, 0.0);
        let mut reordered = Vec::with_capacity(indices.len());
        let mut patches = Vec::with_capacity(meshlets.len());
        for meshlet in meshlets.iter() {
            let start = u32::try_from(reordered.len())?;
            for &index in meshlet.triangles {
                reordered.push(meshlet.vertices[index as usize]);
            }
            let end = u32::try_from(reordered.len())?;
            let patch_indices = &reordered[start as usize..end as usize];
            let (vertices, bounds, cone) = TrianglePatch::measure(&geometry, patch_indices);
            patches.push(TrianglePatch {
                indices: start..end,
                vertices,
                bounds,
                cone,
            });
        }
        ensure!(
            reordered.len() == indices.len(),
            "patch partition changed triangle count"
        );
        geometry.indices = IndexBuffer::new(&reordered)?;
        let res = Self {
            geometry,
            error,
            patches: patches.into_boxed_slice(),
        };
        res.validate()?;
        Ok(res)
    }

    pub fn geometry(&self) -> &Geometry {
        &self.geometry
    }

    pub fn error(&self) -> f32 {
        self.error
    }

    pub fn patches(&self) -> &[TrianglePatch] {
        &self.patches
    }

    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.error.is_finite() && self.error >= 0.0,
            "lod error must be finite and nonnegative"
        );
        let indices = self.geometry.indices.as_u32();
        let mut end = 0;
        for patch in &self.patches {
            ensure!(
                patch.indices.start == end
                    && patch.indices.end > end
                    && patch.indices.end as usize <= indices.len(),
                "invalid patch index range"
            );
            ensure!(
                patch.indices.start.is_multiple_of(3)
                    && patch.indices.end.is_multiple_of(3)
                    && patch.indices.len() <= 128 * 3,
                "invalid patch triangle range"
            );
            let indices = &indices[patch.indices.start as usize..patch.indices.end as usize];
            let mut unique = indices.to_vec();
            unique.sort_unstable();
            unique.dedup();
            ensure!(unique.len() <= 64, "patch exceeds vertex limit");
            let (vertices, bounds, _) = TrianglePatch::measure(&self.geometry, indices);
            ensure!(
                patch.vertices == vertices,
                "invalid patch enclosing vertex range"
            );
            for axis in 0..3 {
                ensure!(
                    patch.bounds.min[axis].is_finite()
                        && patch.bounds.max[axis].is_finite()
                        && patch.bounds.min[axis] <= bounds.min[axis]
                        && patch.bounds.max[axis] >= bounds.max[axis],
                    "patch bounds do not enclose geometry"
                );
            }
            if let Some(cone) = patch.cone {
                let axis = DVec3::from_array(cone.axis.map(f64::from));
                ensure!(
                    axis.is_finite()
                        && (axis.length() - 1.0).abs() < 1e-5
                        && cone.min_dot.is_finite()
                        && cone.min_dot > 0.0
                        && cone.min_dot <= 1.0,
                    "invalid patch face cone"
                );
                let axis = axis.normalize();
                for triangle in indices.chunks_exact(3) {
                    let normal = TrianglePatch::face_normal(&self.geometry, triangle);
                    ensure!(
                        normal.is_some_and(|normal| normal.dot(axis) >= cone.min_dot as f64),
                        "patch cone does not enclose geometric faces"
                    );
                }
            }
            end = patch.indices.end;
        }
        ensure!(
            end as usize == indices.len(),
            "patches must partition every lod triangle"
        );
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Lod {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct LodData {
            geometry: Geometry,
            error: f32,
            patches: Box<[TrianglePatch]>,
        }
        let data = LodData::deserialize(deserializer)?;
        let res = Self {
            geometry: data.geometry,
            error: data.error,
            patches: data.patches,
        };
        res.validate().map_err(D::Error::custom)?;
        Ok(res)
    }
}

/// Producing metric, not a visibility, silhouette or maximum-displacement certificate.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum LodMetric {
    SourceOnly,
    GeometricAbsolute,
    AttributeWeightedAbsolute,
}

/// Optional generic producer facts. A caller's purpose or profile name does not belong here.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LodProvenance {
    pub producer: String,
    pub settings: String,
    pub stop_reason: String,
}

/// One uniform-layout chain, keyed by its vertex type. Level zero is the submitted input.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LodSet {
    metric: LodMetric,
    levels: Box<[Lod]>,
    provenance: Option<LodProvenance>,
}

impl LodSet {
    pub fn new(metric: LodMetric, levels: Box<[Lod]>) -> anyhow::Result<Self> {
        ensure!(!levels.is_empty(), "lod set must contain its source level");
        ensure!(levels[0].error == 0.0, "source level error must be zero");
        ensure!(
            metric != LodMetric::SourceOnly || levels.len() == 1,
            "source-only set cannot contain reductions"
        );
        let layout = levels[0].geometry.vertex_type();
        let mut previous = usize::MAX;
        for lod in &levels {
            lod.validate()?;
            ensure!(
                lod.geometry.vertex_type() == layout,
                "lod set mixes vertex layouts"
            );
            let count = lod.geometry.indices().index_count();
            ensure!(
                count < previous,
                "lod chain must strictly reduce triangle count"
            );
            previous = count;
        }
        Ok(Self {
            metric,
            levels,
            provenance: None,
        })
    }

    pub fn levels(&self) -> &[Lod] {
        &self.levels
    }
    pub fn metric(&self) -> LodMetric {
        self.metric
    }
    pub fn vertex_type(&self) -> VertexType {
        self.levels[0].geometry.vertex_type()
    }
    pub fn provenance(&self) -> Option<&LodProvenance> {
        self.provenance.as_ref()
    }
    pub fn with_provenance(mut self, provenance: LodProvenance) -> Self {
        self.provenance = Some(provenance);
        self
    }
}

impl<'de> Deserialize<'de> for LodSet {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Data {
            metric: LodMetric,
            levels: Box<[Lod]>,
            provenance: Option<LodProvenance>,
        }
        let data = Data::deserialize(deserializer)?;
        let mut set = Self::new(data.metric, data.levels).map_err(D::Error::custom)?;
        set.provenance = data.provenance;
        Ok(set)
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Mesh {
    blob: Option<BlobId>,
    pub data: DataMap,
    primitives: Vec<Primitive>,
    skin: Option<Skin>,
}

impl Mesh {
    pub fn new(primitives: Vec<Primitive>, skin: Option<Skin>) -> anyhow::Result<Self> {
        let res = Self {
            blob: None,
            data: DataMap::default(),
            primitives,
            skin,
        };
        res.validate()?;
        Ok(res)
    }

    pub fn blob(&self) -> Option<BlobId> {
        self.blob
    }

    pub fn data(&self, key: &str) -> Option<DataRef<'_>> {
        self.data.get(key)
    }

    pub fn primitives(&self) -> &[Primitive] {
        &self.primitives
    }

    pub fn primitives_mut(&mut self) -> &mut [Primitive] {
        &mut self.primitives
    }

    pub fn set_blob(&mut self, id: BlobId) {
        self.blob = Some(id);
    }

    pub fn skin(&self) -> Option<&Skin> {
        self.skin.as_ref()
    }

    fn validate(&self) -> anyhow::Result<()> {
        for primitive in &self.primitives {
            primitive.validate()?;
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Mesh {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct MeshData {
            blob: Option<BlobId>,
            data: DataMap,
            primitives: Vec<Primitive>,
            skin: Option<Skin>,
        }
        let data = MeshData::deserialize(deserializer)?;
        let res = Self {
            blob: data.blob,
            data: data.data,
            primitives: data.primitives,
            skin: data.skin,
        };
        res.validate().map_err(D::Error::custom)?;
        Ok(res)
    }
}

#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Primitive {
    base: Geometry,
    material: u8,
    lods: Box<[LodSet]>,
}

impl Primitive {
    /// Creates a primitive with its source geometry and no alternative layouts.
    pub fn new(material: u8, base: Geometry) -> anyhow::Result<Self> {
        let res = Self {
            base,
            material,
            lods: Box::new([]),
        };
        res.validate()?;
        Ok(res)
    }

    pub fn base(&self) -> &Geometry {
        &self.base
    }

    pub fn material(&self) -> u8 {
        self.material
    }

    pub fn lod_sets(&self) -> &[LodSet] {
        &self.lods
    }

    pub fn lod_set(&self, layout: VertexType) -> Option<&LodSet> {
        self.lods
            .binary_search_by_key(&layout.bits(), |set| set.vertex_type().bits())
            .ok()
            .map(|idx| &self.lods[idx])
    }

    pub fn set_lods(&mut self, mut lods: Box<[LodSet]>) -> anyhow::Result<()> {
        lods.sort_by_key(|set| set.vertex_type().bits());
        ensure!(
            lods.windows(2)
                .all(|pair| pair[0].vertex_type() != pair[1].vertex_type()),
            "duplicate lod vertex layout"
        );
        self.lods = lods;
        Ok(())
    }

    fn validate(&self) -> anyhow::Result<()> {
        ensure!(
            self.lods
                .windows(2)
                .all(|pair| pair[0].vertex_type().bits() < pair[1].vertex_type().bits()),
            "lod vertex layouts must be unique and sorted"
        );
        Ok(())
    }
}

impl<'de> Deserialize<'de> for Primitive {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct PrimitiveData {
            base: Geometry,
            material: u8,
            lods: Box<[LodSet]>,
        }
        let data = PrimitiveData::deserialize(deserializer)?;
        let res = Self {
            base: data.base,
            material: data.material,
            lods: data.lods,
        };
        res.validate().map_err(D::Error::custom)?;
        Ok(res)
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Skin {
    joints: Box<[Joint]>,
}

impl Skin {
    #[cfg(feature = "bake")]
    pub(super) fn new(joints: impl Into<Box<[Joint]>>) -> Self {
        let joints = joints.into();
        debug_assert!(!joints.is_empty());
        Self { joints }
    }

    pub fn joints(&self) -> &[Joint] {
        &self.joints
    }
}

/// Owned structurally by Primitive -> layout set -> LOD -> patch. Ranges use
/// index and vertex elements (not bytes), and share the owning LOD's buffers.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct TrianglePatch {
    indices: Range<u32>,
    vertices: Range<u32>,
    bounds: Bounds,
    cone: Option<FaceCone>,
}

impl TrianglePatch {
    pub fn indices(&self) -> Range<u32> {
        self.indices.clone()
    }

    /// Enclosing min..max+1 range; not a dense or exclusive vertex allocation.
    pub fn vertices(&self) -> Range<u32> {
        self.vertices.clone()
    }

    pub fn bounds(&self) -> Bounds {
        self.bounds
    }

    /// None for degenerate faces or cones too wide for conservative rejection.
    pub fn cone(&self) -> Option<FaceCone> {
        self.cone
    }

    fn face_normal(geometry: &Geometry, triangle: &[u32]) -> Option<DVec3> {
        let a = geometry.position(triangle[0]);
        let b = geometry.position(triangle[1]);
        let c = geometry.position(triangle[2]);
        (b - a).cross(c - a).try_normalize()
    }

    fn measure(geometry: &Geometry, indices: &[u32]) -> (Range<u32>, Bounds, Option<FaceCone>) {
        let mut min = DVec3::splat(f64::INFINITY);
        let mut max = DVec3::splat(f64::NEG_INFINITY);
        let mut first = u32::MAX;
        let mut last = 0;
        for &index in indices {
            let position = geometry.position(index);
            min = min.min(position);
            max = max.max(position);
            first = first.min(index);
            last = last.max(index);
        }
        let normals = indices
            .chunks_exact(3)
            .map(|triangle| Self::face_normal(geometry, triangle))
            .collect::<Option<Vec<_>>>();
        let cone = normals.and_then(|normals| {
            let axis = normals.iter().sum::<DVec3>().try_normalize()?;
            let axis = axis.to_array().map(|value| value as f32);
            let normalized = DVec3::from_array(axis.map(f64::from)).normalize();
            let min_dot = normals
                .iter()
                .map(|normal| normal.dot(normalized))
                .fold(1.0_f64, f64::min);
            // Pad the measured cone, including f64 arithmetic and stored-axis quantization.
            let min_dot = ((min_dot - 8.0 * f32::EPSILON as f64) as f32).next_down();
            (min_dot > 0.0).then_some(FaceCone { axis, min_dot })
        });
        (
            first..last + 1,
            Bounds {
                min: min.to_array().map(|value| value as f32),
                max: max.to_array().map(|value| value as f32),
            },
            cone,
        )
    }
}

bitflags! {
    #[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
    pub struct VertexType: u8 {
        const POSITION = 1 << 0;
        const JOINTS_WEIGHTS = Self::POSITION.bits() | (1 << 1);
        const NORMAL = Self::POSITION.bits() | (1 << 2);
        const TANGENT = Self::POSITION.bits() | (1 << 3);
        const TEXTURE0 = Self::POSITION.bits() | (1 << 4);
        const TEXTURE1 = Self::POSITION.bits() | (1 << 5);

        /// Octahedral normal, two native-endian signed normalized i16 components.
        const PACKED_NORMAL = Self::POSITION.bits() | (1 << 6);
    }
}

impl VertexType {
    pub fn stride(&self) -> usize {
        let mut res = 12;
        if self.contains(Self::NORMAL) {
            res += 12;
        }
        if self.contains(Self::PACKED_NORMAL) {
            res += 4;
        }
        if self.contains(Self::TEXTURE0) {
            res += 8;
        }
        if self.contains(Self::TEXTURE1) {
            res += 8;
        }
        if self.contains(Self::TANGENT) {
            res += 16;
        }
        if self.contains(Self::JOINTS_WEIGHTS) {
            res += 8;
        }
        res
    }

    pub(crate) fn validate(self) -> anyhow::Result<()> {
        ensure!(
            self.bits() & !Self::all().bits() == 0,
            "unknown vertex layout bits"
        );
        ensure!(
            self.contains(Self::POSITION),
            "vertex layout must include positions"
        );
        if self.contains(Self::PACKED_NORMAL) {
            ensure!(
                !self.contains(Self::NORMAL),
                "vertex layout cannot combine normal and packed normal"
            );
            ensure!(
                (self & !(Self::PACKED_NORMAL | Self::TEXTURE0)).is_empty(),
                "packed normal layout only supports positions and texture0"
            );
        }

        // Authored tangents remain valid without the UVs used to generate them.
        ensure!(
            !self.contains(Self::TANGENT) || self.contains(Self::NORMAL),
            "tangent layout requires normal"
        );
        Ok(())
    }
}

impl<'de> Deserialize<'de> for VertexType {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        let res: Self = bitflags::serde::deserialize(deserializer)?;
        res.validate().map_err(D::Error::custom)?;
        Ok(res)
    }
}

#[cfg(test)]
pub(crate) mod test {
    use {
        super::{
            Geometry, Lod, LodMetric, LodSet, Mesh, Primitive, Skin, TrianglePatch, VertexType,
        },
        crate::index::IndexBuffer,
        glam::DVec3,
        serde::{Serialize, de::DeserializeOwned},
    };

    pub(crate) fn grid(side: u32, curved: bool) -> Geometry {
        let mut vertices = Vec::new();
        for y in 0..side {
            for x in 0..side {
                let x = x as f32;
                let y = y as f32;
                let z = if curved { (x * x + y * y) * 0.02 } else { 0.0 };
                let normal = if curved {
                    glam::vec3(-0.04 * x, -0.04 * y, 1.0).normalize()
                } else {
                    glam::Vec3::Z
                };
                for value in [
                    x,
                    y,
                    z,
                    normal.x,
                    normal.y,
                    normal.z,
                    x / side as f32,
                    y / side as f32,
                ] {
                    vertices.extend_from_slice(&value.to_ne_bytes());
                }
            }
        }
        let mut indices = Vec::new();
        for y in 0..side - 1 {
            for x in 0..side - 1 {
                let i = y * side + x;
                indices.extend([i, i + 1, i + side, i + 1, i + side + 1, i + side]);
            }
        }
        Geometry::new(
            &vertices,
            VertexType::NORMAL | VertexType::TEXTURE0,
            IndexBuffer::new(&indices).unwrap(),
        )
        .unwrap()
    }

    // Compare oriented triangles independent of vertex remaps and cyclic corner rotation.
    pub(crate) fn triangles(geometry: &Geometry, texture0: bool) -> Vec<[[u32; 5]; 3]> {
        let uv = geometry.texture0().map(|uv| uv.collect::<Vec<_>>());
        let mut triangles = geometry
            .indices()
            .as_u32()
            .chunks_exact(3)
            .map(|triangle| {
                let vertices = std::array::from_fn(|corner| {
                    let index = triangle[corner];
                    let p = geometry
                        .position(index)
                        .to_array()
                        .map(|value| (value as f32).to_bits());
                    let uv = if texture0 {
                        uv.as_ref().unwrap()[index as usize].map(f32::to_bits)
                    } else {
                        [0; 2]
                    };
                    [p[0], p[1], p[2], uv[0], uv[1]]
                });
                let [a, b, c] = vertices;
                vertices.min([b, c, a]).min([c, a, b])
            })
            .collect::<Vec<_>>();
        triangles.sort_unstable();
        triangles
    }

    fn rejects<T: Serialize + DeserializeOwned>(invalid: &T) {
        let encoded = bincode::serde::encode_to_vec(invalid, bincode::config::legacy()).unwrap();
        assert!(
            bincode::serde::decode_from_slice::<T, _>(&encoded, bincode::config::legacy()).is_err()
        );
    }

    #[test]
    fn geometry_rejects_invalid_layouts_pairs_and_nonfinite_positions() {
        for vertex_type in [
            VertexType::empty(),
            VertexType::from_bits_retain(1 << 2),
            VertexType::from_bits_retain(0x81),
            VertexType::NORMAL | VertexType::PACKED_NORMAL,
            VertexType::PACKED_NORMAL | VertexType::JOINTS_WEIGHTS,
            VertexType::PACKED_NORMAL | VertexType::TEXTURE1,
            VertexType::TANGENT,
        ] {
            let invalid = Geometry {
                vertex_buf: vec![0; vertex_type.stride()],
                vertex_type,
                indices: IndexBuffer::new(&[0, 0, 0]).unwrap(),
            };
            assert!(
                Geometry::new(&invalid.vertex_buf, vertex_type, invalid.indices.clone()).is_err()
            );
            rejects(&invalid);
            rejects(&vertex_type);
        }
        for vertex_buf in [
            vec![],
            vec![0; 13],
            f32::NAN.to_ne_bytes().repeat(3),
            f32::INFINITY.to_ne_bytes().repeat(3),
        ] {
            let invalid = Geometry {
                vertex_buf,
                vertex_type: VertexType::POSITION,
                indices: IndexBuffer::new(&[0, 0, 0]).unwrap(),
            };
            assert!(
                Geometry::new(
                    &invalid.vertex_buf,
                    invalid.vertex_type,
                    invalid.indices.clone()
                )
                .is_err()
            );
            rejects(&invalid);
        }
        let invalid = Geometry {
            vertex_buf: vec![0; 12],
            vertex_type: VertexType::POSITION,
            indices: IndexBuffer::new(&[0, 0, 1]).unwrap(),
        };
        assert!(
            Geometry::new(
                &invalid.vertex_buf,
                invalid.vertex_type,
                invalid.indices.clone()
            )
            .is_err()
        );
        rejects(&invalid);
    }

    #[test]
    fn compact_layout_strides_uv_offsets_and_canonical_rejection() {
        for (vertex_type, stride, uv_offset) in [
            (VertexType::POSITION, 12, None),
            (VertexType::PACKED_NORMAL, 16, None),
            (VertexType::TEXTURE0, 20, Some(12)),
            (
                VertexType::PACKED_NORMAL | VertexType::TEXTURE0,
                24,
                Some(16),
            ),
            (VertexType::NORMAL | VertexType::TEXTURE0, 32, Some(24)),
        ] {
            assert_eq!(vertex_type.stride(), stride);
            let mut vertices = vec![0; stride * 3];
            if let Some(offset) = uv_offset {
                for vertex in vertices.chunks_exact_mut(stride) {
                    vertex[offset..offset + 4].copy_from_slice(&0.25_f32.to_ne_bytes());
                    vertex[offset + 4..offset + 8].copy_from_slice(&0.75_f32.to_ne_bytes());
                }
            }
            let geometry = Geometry::new(
                &vertices,
                vertex_type,
                IndexBuffer::new(&[0, 1, 2]).unwrap(),
            )
            .unwrap();
            assert_eq!(geometry.vertex_count(), 3);
            assert_eq!(
                geometry.texture0().map(|uv| uv.collect::<Vec<_>>()),
                uv_offset.map(|_| vec![[0.25, 0.75]; 3])
            );
            let primitive = Primitive::new(4, geometry.clone());
            assert!(primitive.is_ok());
        }
        assert!(!VertexType::PACKED_NORMAL.contains(VertexType::NORMAL));
        assert_eq!(VertexType::NORMAL.stride(), 24);
    }

    #[test]
    fn patches_cover_triangles_without_duplicating_vertices_and_enclose_faces() {
        for curved in [false, true] {
            let source = grid(17, curved);
            let lod = Lod::new(source.clone(), 0.0).unwrap();
            assert!(lod.patches().len() > 1);
            assert_eq!(lod.geometry().vertex_data(), source.vertex_data());
            assert_eq!(triangles(lod.geometry(), true), triangles(&source, true));
            let indices = lod.geometry().indices().as_u32();
            let mut end = 0;
            let mut rejected = 0;
            for patch in lod.patches() {
                assert_eq!(patch.indices().start, end);
                end = patch.indices().end;
                let indices = &indices[patch.indices().start as usize..end as usize];
                assert!(indices.len() <= 128 * 3);
                for &index in indices {
                    assert!(patch.vertices().contains(&index));
                    let position = lod.geometry().position(index);
                    assert!(
                        position
                            .cmpge(DVec3::from_array(patch.bounds().min().map(f64::from)))
                            .all()
                    );
                    assert!(
                        position
                            .cmple(DVec3::from_array(patch.bounds().max().map(f64::from)))
                            .all()
                    );
                }
                let cone = patch.cone().unwrap();
                let axis = DVec3::from_array(cone.axis().map(f64::from)).normalize();
                let sine = (1.0 - (cone.min_dot() as f64).powi(2)).sqrt();
                for x in -2..=2 {
                    for y in -2..=2 {
                        for z in -2..=2 {
                            let Some(view) =
                                DVec3::new(x as f64, y as f64, z as f64).try_normalize()
                            else {
                                continue;
                            };
                            if view.dot(axis) < -sine {
                                rejected += 1;
                                for triangle in indices.chunks_exact(3) {
                                    assert!(
                                        TrianglePatch::face_normal(lod.geometry(), triangle)
                                            .unwrap()
                                            .dot(view)
                                            < 0.0
                                    );
                                }
                            }
                        }
                    }
                }
            }
            assert_eq!(end as usize, indices.len());
            assert!(rejected > 0);
            let encoded = bincode::serde::encode_to_vec(&lod, bincode::config::legacy()).unwrap();
            let (decoded, len): (Lod, _) =
                bincode::serde::decode_from_slice(&encoded, bincode::config::legacy()).unwrap();
            assert_eq!(len, encoded.len());
            assert_eq!(decoded, lod);
        }
    }

    #[test]
    fn patch_sparse_ranges_and_invalid_cones_are_conservative() {
        let source = grid(3, false);
        for indices in [[2, 5, 4, 2, 4, 5], [2, 5, 4, 2, 2, 2]] {
            let geometry = Geometry::new(
                source.vertex_data(),
                source.vertex_type(),
                IndexBuffer::new(&indices).unwrap(),
            )
            .unwrap();
            let lod = Lod::new(geometry, 0.0).unwrap();
            assert_eq!(lod.patches().len(), 1);
            assert_eq!(lod.patches()[0].vertices(), 2..6);
            assert!(lod.patches()[0].cone().is_none());
        }
    }

    #[test]
    fn deserialize_rejects_malformed_lod_bounds_patches_cones_and_errors() {
        let valid = Lod::new(grid(17, true), 0.0).unwrap();
        for mutate in [
            (|lod: &mut Lod| lod.patches = Box::new([])) as fn(&mut Lod),
            |lod| lod.patches[0].indices.start = 3,
            |lod| lod.patches[0].indices.end = u32::MAX,
            |lod| lod.patches[0].indices.end -= 1,
            |lod| lod.patches[1].indices.start = 0,
            |lod| lod.patches[0].vertices.end -= 1,
            |lod| lod.patches[0].bounds.min[0] = f32::NAN,
            |lod| lod.patches[0].bounds.max[0] = -1.0,
            |lod| lod.patches[0].cone.as_mut().unwrap().axis = [0.0; 3],
            |lod| lod.patches[0].cone.as_mut().unwrap().axis = [0.0, 0.0, -1.0],
            |lod| lod.patches[0].cone.as_mut().unwrap().min_dot = 1.0,
            |lod| lod.error = f32::NAN,
            |lod| lod.error = -1.0,
        ] {
            let mut invalid = valid.clone();
            mutate(&mut invalid);
            assert!(invalid.validate().is_err());
            rejects(&invalid);
        }
        for error in [-1.0, f32::INFINITY, f32::NAN] {
            assert!(Lod::new(grid(2, false), error).is_err());
        }
        assert!(Lod::new(grid(2, false), 0.01).is_ok());
    }

    #[test]
    fn producing_errors_remain_individual_and_chains_require_strict_reduction() {
        let source = grid(3, false);
        let indices = source.indices().as_u32();
        let lods = [0.0, 2.0, 1.0]
            .into_iter()
            .enumerate()
            .map(|(level, error)| {
                let geometry = Geometry::new(
                    source.vertex_data(),
                    source.vertex_type(),
                    IndexBuffer::new(&indices[..indices.len() - level * 3]).unwrap(),
                )
                .unwrap();
                Lod::new(geometry, error).unwrap()
            })
            .collect::<Box<_>>();
        let chain = LodSet::new(LodMetric::GeometricAbsolute, lods).unwrap();
        assert_eq!(
            chain.levels().iter().map(Lod::error).collect::<Vec<_>>(),
            [0.0, 2.0, 1.0]
        );
        let mut invalid = chain.clone();
        invalid.levels[1] = invalid.levels[0].clone();
        assert!(LodSet::new(invalid.metric, invalid.levels.clone()).is_err());
        rejects(&invalid);
        let encoded = bincode::serde::encode_to_vec(&chain, bincode::config::legacy()).unwrap();
        let (decoded, _): (LodSet, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::legacy()).unwrap();
        assert_eq!(decoded, chain);
    }

    #[test]
    fn layout_sets_do_not_impose_consumer_skin_policy() {
        let base = grid(2, false);
        let positions = base
            .vertex_data()
            .chunks_exact(base.vertex_type().stride())
            .flat_map(|vertex| vertex[..12].iter().copied())
            .collect::<Vec<_>>();
        let geometry =
            Geometry::new(&positions, VertexType::POSITION, base.indices().clone()).unwrap();
        let lod = Lod::new(geometry, 0.0).unwrap();
        let primitive = Primitive {
            base,
            material: 3,
            lods: vec![LodSet::new(LodMetric::SourceOnly, vec![lod].into_boxed_slice()).unwrap()]
                .into_boxed_slice(),
        };
        primitive.validate().unwrap();
        let mesh = Mesh {
            blob: None,
            data: Default::default(),
            primitives: vec![primitive.clone()],
            skin: Some(Skin {
                joints: Box::new([]),
            }),
        };
        mesh.validate().unwrap();
        let encoded = bincode::serde::encode_to_vec(&mesh, bincode::config::legacy()).unwrap();
        let (_decoded, _): (Mesh, _) =
            bincode::serde::decode_from_slice(&encoded, bincode::config::legacy()).unwrap();
        let mut primitive = primitive;
        primitive.base = Geometry::new(
            &[0; 80],
            VertexType::JOINTS_WEIGHTS,
            primitive.base.indices().clone(),
        )
        .unwrap();
        primitive.validate().unwrap();
        assert!(Lod::new(primitive.base, 0.0).is_ok());
    }

    #[test]
    fn layout_keys_are_unique_and_source_only_does_not_imply_other_variants() {
        let base = grid(3, false);
        let mut primitive = Primitive::new(0, base.clone()).unwrap();
        let set = |layout| {
            LodSet::new(
                LodMetric::SourceOnly,
                vec![Lod::new(base.project(layout).unwrap(), 0.0).unwrap()].into_boxed_slice(),
            )
            .unwrap()
        };
        primitive
            .set_lods(
                vec![set(VertexType::PACKED_NORMAL), set(VertexType::POSITION)].into_boxed_slice(),
            )
            .unwrap();
        assert!(
            primitive
                .lod_set(VertexType::PACKED_NORMAL | VertexType::TEXTURE0)
                .is_none()
        );
        assert!(
            primitive
                .set_lods(
                    vec![set(VertexType::POSITION), set(VertexType::POSITION)].into_boxed_slice()
                )
                .is_err()
        );
        let mut duplicate = primitive.clone();
        duplicate.lods =
            vec![set(VertexType::POSITION), set(VertexType::POSITION)].into_boxed_slice();
        rejects(&duplicate);
        let mixed = vec![
            Lod::new(base.project(VertexType::POSITION).unwrap(), 0.0).unwrap(),
            Lod::new(
                grid(2, false).project(VertexType::PACKED_NORMAL).unwrap(),
                1.0,
            )
            .unwrap(),
        ];
        assert!(LodSet::new(LodMetric::GeometricAbsolute, mixed.into_boxed_slice()).is_err());
    }
}
