use {
    super::{Bounds, Geometry, Lod, LodMetric, LodProvenance, Primitive, VertexType},
    anyhow::{Context as _, ensure},
    serde::{Deserialize, Deserializer, Serialize, de::Error as _},
    std::collections::BTreeMap,
};

/// Stable index into the canonical primitives of one mesh.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Ord, PartialEq, PartialOrd, Serialize)]
pub struct PrimitiveId(pub u32);

impl PrimitiveId {
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One immutable complete alternative. Absent members have zero triangles, not fallback geometry.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct GroupSnapshot {
    partitions: Box<[LodPartition]>,
    error: f32,
    triangle_count: usize,
    bounds: Bounds,
    diagnostics: SnapshotDiagnostics,
}

impl GroupSnapshot {
    pub fn new(
        partitions: Box<[LodPartition]>,
        diagnostics: SnapshotDiagnostics,
    ) -> anyhow::Result<Self> {
        let first = partitions.first().context("empty grouped snapshot")?;
        let error = first.lod.error();
        ensure!(
            partitions
                .windows(2)
                .all(|p| p[0].primitive < p[1].primitive),
            "snapshot members must be unique and sorted"
        );
        let mut min = [f32::INFINITY; 3];
        let mut max = [f32::NEG_INFINITY; 3];
        let mut triangle_count = 0usize;
        for partition in &partitions {
            partition.lod.validate()?;
            let geometry = partition.lod.geometry();
            let layout = geometry.vertex_type();
            ensure!(
                !layout.contains(VertexType::JOINTS_WEIGHTS),
                "grouped snapshots require static geometry"
            );
            let attribute_offset = if layout.contains(VertexType::PACKED_NORMAL) {
                16
            } else {
                12
            };
            for vertex in geometry.vertex_data().chunks_exact(layout.stride()) {
                ensure!(
                    vertex[attribute_offset..]
                        .chunks_exact(4)
                        .all(|value| f32::from_ne_bytes(value.try_into().unwrap()).is_finite()),
                    "snapshot attributes must be finite"
                );
            }
            ensure!(
                partition.lod.error() == error,
                "snapshot partitions disagree on error"
            );
            triangle_count = triangle_count
                .checked_add(partition.lod.geometry().indices().triangle_count())
                .context("snapshot triangle count overflow")?;
            for patch in partition.lod.patches() {
                for axis in 0..3 {
                    min[axis] = min[axis].min(patch.bounds().min()[axis]);
                    max[axis] = max[axis].max(patch.bounds().max()[axis]);
                }
            }
        }
        ensure!(
            diagnostics.max_projection_distance.is_finite()
                && diagnostics.max_projection_distance >= 0.0
                && diagnostics.mixed_material_triangles <= triangle_count
                && diagnostics.ambiguous_samples <= triangle_count.saturating_mul(7)
                && diagnostics.transferred_corners <= triangle_count.saturating_mul(3)
                && diagnostics.uv_chart_conflicts <= diagnostics.transferred_corners,
            "invalid snapshot correspondence diagnostics"
        );
        Ok(Self {
            partitions,
            error,
            triangle_count,
            bounds: Bounds { min, max },
            diagnostics,
        })
    }

    pub fn partitions(&self) -> &[LodPartition] {
        &self.partitions
    }
    pub fn partition(&self, primitive: PrimitiveId) -> Option<&Lod> {
        self.partitions
            .binary_search_by_key(&primitive, |p| p.primitive)
            .ok()
            .map(|idx| &self.partitions[idx].lod)
    }
    pub fn error(&self) -> f32 {
        self.error
    }
    pub fn triangle_count(&self) -> usize {
        self.triangle_count
    }
    pub fn bounds(&self) -> Bounds {
        self.bounds
    }
    pub fn diagnostics(&self) -> &SnapshotDiagnostics {
        &self.diagnostics
    }
}

impl<'de> Deserialize<'de> for GroupSnapshot {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Data {
            partitions: Box<[LodPartition]>,
            error: f32,
            triangle_count: usize,
            bounds: Bounds,
            diagnostics: SnapshotDiagnostics,
        }
        let data = Data::deserialize(deserializer)?;
        let snapshot = Self::new(data.partitions, data.diagnostics).map_err(D::Error::custom)?;
        if snapshot.error != data.error
            || snapshot.triangle_count != data.triangle_count
            || snapshot.bounds != data.bounds
        {
            return Err(D::Error::custom(
                "snapshot metadata differs from finalized geometry",
            ));
        }
        Ok(snapshot)
    }
}

/// A uniform-layout chain selected atomically across its original material primitives.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct LodGroup {
    members: Box<[LodGroupMember]>,
    metric: LodMetric,
    snapshots: Box<[GroupSnapshot]>,
    preparation: Option<LodPreparation>,
    provenance: LodProvenance,
}

impl LodGroup {
    pub fn new(
        members: Box<[LodGroupMember]>,
        metric: LodMetric,
        snapshots: Box<[GroupSnapshot]>,
        preparation: Option<LodPreparation>,
        provenance: LodProvenance,
    ) -> anyhow::Result<Self> {
        ensure!(
            !members.is_empty() && members.windows(2).all(|m| m[0].primitive < m[1].primitive),
            "group members must be nonempty, unique and sorted"
        );
        let source = snapshots
            .first()
            .context("group must contain its source snapshot")?;
        ensure!(
            source.error() == 0.0 && source.partitions.len() == members.len(),
            "invalid group source snapshot"
        );
        ensure!(
            metric != LodMetric::SourceOnly || snapshots.len() == 1,
            "source-only group contains reductions"
        );
        if let Some(preparation) = &preparation {
            ensure!(
                preparation.error.is_finite()
                    && preparation.error >= 0.0
                    && preparation.working_triangles > 0,
                "invalid group preparation diagnostics"
            );
            ensure!(
                snapshots.iter().skip(1).all(|snapshot| {
                    snapshot.error() >= preparation.error
                        && snapshot.triangle_count() <= preparation.working_triangles
                }),
                "group reduction disagrees with preparation"
            );
        }
        ensure!(
            snapshots.len() == 1
                || !matches!(
                    metric,
                    LodMetric::SampledGeometricAbsolute
                        | LodMetric::SampledAttributeWeightedAbsolute
                )
                || preparation.is_some(),
            "sampled reductions require preparation diagnostics"
        );
        let layout = source.partitions[0].lod.geometry().vertex_type();
        let mut previous = usize::MAX;
        for snapshot in &snapshots {
            ensure!(
                snapshot.triangle_count < previous,
                "group snapshots must strictly reduce aggregate triangle count"
            );
            previous = snapshot.triangle_count;
            for part in &snapshot.partitions {
                ensure!(
                    members
                        .binary_search_by_key(&part.primitive, |m| m.primitive)
                        .is_ok(),
                    "snapshot references unknown group member"
                );
                ensure!(
                    part.lod.geometry().vertex_type() == layout,
                    "group mixes vertex layouts"
                );
            }
        }
        Ok(Self {
            members,
            metric,
            snapshots,
            preparation,
            provenance,
        })
    }

    pub fn members(&self) -> &[LodGroupMember] {
        &self.members
    }
    pub fn metric(&self) -> LodMetric {
        self.metric
    }
    pub fn snapshots(&self) -> &[GroupSnapshot] {
        &self.snapshots
    }
    pub fn vertex_type(&self) -> VertexType {
        self.snapshots[0].partitions[0].lod.geometry().vertex_type()
    }
    pub fn provenance(&self) -> &LodProvenance {
        &self.provenance
    }
    /// Preparation facts survive rejection of every reduced candidate. `None` means that
    /// preparation did not finish, not a measured zero deviation or zero reconstruction work.
    pub fn preparation(&self) -> Option<&LodPreparation> {
        self.preparation.as_ref()
    }
    /// Prefix-maximum selection metric for one complete snapshot, not a per-partition budget.
    pub fn selection_error(&self, ordinal: usize) -> Option<f32> {
        self.snapshots.get(ordinal)?;
        Some(
            self.snapshots[..=ordinal]
                .iter()
                .map(GroupSnapshot::error)
                .fold(0.0, f32::max),
        )
    }

    pub fn validate_source(&self, primitives: &[Primitive]) -> anyhow::Result<()> {
        for member in &self.members {
            let source = primitives
                .get(member.primitive.index())
                .context("group primitive index out of bounds")?;
            ensure!(
                !source
                    .base()
                    .vertex_type()
                    .contains(VertexType::JOINTS_WEIGHTS),
                "grouped lod source must be static"
            );
            ensure!(
                source.material() == member.material
                    && identity(source.base()) == member.source_identity,
                "group source association mismatch"
            );
            let projected = source.base().project(self.vertex_type())?;
            let first = self.snapshots[0]
                .partition(member.primitive)
                .context("missing source partition")?
                .geometry();
            ensure!(
                equivalent(&projected, first),
                "group source geometry changed"
            );
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for LodGroup {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        struct Data {
            members: Box<[LodGroupMember]>,
            metric: LodMetric,
            snapshots: Box<[GroupSnapshot]>,
            preparation: Option<LodPreparation>,
            provenance: LodProvenance,
        }
        let data = Data::deserialize(deserializer)?;
        Self::new(
            data.members,
            data.metric,
            data.snapshots,
            data.preparation,
            data.provenance,
        )
        .map_err(D::Error::custom)
    }
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct LodGroupMember {
    primitive: PrimitiveId,
    material: u8,
    source_identity: u64,
    reference_identity: u64,
}

impl LodGroupMember {
    /// Identities are association checksums, not authenticity certificates.
    pub fn new(
        primitive: PrimitiveId,
        material: u8,
        source: &Geometry,
        reference: &Geometry,
    ) -> Self {
        Self {
            primitive,
            material,
            source_identity: identity(source),
            reference_identity: identity(reference),
        }
    }
    pub fn primitive(&self) -> PrimitiveId {
        self.primitive
    }
    pub fn material(&self) -> u8 {
        self.material
    }
    pub fn source_identity(&self) -> u64 {
        self.source_identity
    }
    pub fn reference_identity(&self) -> u64 {
        self.reference_identity
    }
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LodPartition {
    primitive: PrimitiveId,
    lod: Lod,
}

impl LodPartition {
    pub fn new(primitive: PrimitiveId, lod: Lod) -> Self {
        Self { primitive, lod }
    }
    pub fn primitive(&self) -> PrimitiveId {
        self.primitive
    }
    pub fn lod(&self) -> &Lod {
        &self.lod
    }
}

/// Measurements of the prepared working surface, independent of published snapshots.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct LodPreparation {
    /// Canonical-relative preparation deviation in mesh-local distance units.
    pub error: f32,
    pub voxel_cells: usize,
    pub working_triangles: usize,
}

/// Deterministic build facts, measured before final triangle reordering.
#[derive(Clone, Debug, Default, Deserialize, PartialEq, Serialize)]
pub struct SnapshotDiagnostics {
    pub mixed_material_triangles: usize,
    pub ambiguous_samples: usize,
    pub transferred_corners: usize,
    pub uv_chart_conflicts: usize,
    pub max_projection_distance: f32,
}

fn identity(source: &Geometry) -> u64 {
    let mut hash = crate::update_hash(crate::FNV_OFFSET, &[source.vertex_type().bits()]);
    hash = crate::update_hash(hash, source.vertex_data());
    for index in source.indices().as_u32() {
        hash = crate::update_hash(hash, &index.to_le_bytes());
    }
    hash
}

fn equivalent(a: &Geometry, b: &Geometry) -> bool {
    if a.vertex_type() != b.vertex_type() || a.vertex_data() != b.vertex_data() {
        return false;
    }
    let faces = |g: &Geometry| {
        let mut counts = BTreeMap::<[u32; 3], usize>::new();
        for f in g.indices().as_u32().chunks_exact(3) {
            let key = [[f[0], f[1], f[2]], [f[1], f[2], f[0]], [f[2], f[0], f[1]]]
                .into_iter()
                .min()
                .unwrap();
            *counts.entry(key).or_default() += 1;
        }
        counts
    };
    faces(a) == faces(b)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{index::IndexBuffer, mesh::Mesh};

    fn geometry(count: usize) -> Geometry {
        let bytes = [0f32, 0., 0., 1., 0., 0., 0., 1., 0.]
            .into_iter()
            .flat_map(f32::to_ne_bytes)
            .collect::<Vec<_>>();
        Geometry::new(
            &bytes,
            VertexType::POSITION,
            IndexBuffer::new(&[0, 1, 2].repeat(count)).unwrap(),
        )
        .unwrap()
    }

    fn fixture() -> (Mesh, LodGroup) {
        let originals = [5, 3, 2].map(geometry);
        let mesh = Mesh::new(
            originals
                .iter()
                .enumerate()
                .map(|(idx, g)| Primitive::new(idx as u8, g.clone()).unwrap())
                .collect(),
            None,
        )
        .unwrap();
        let members = originals
            .iter()
            .enumerate()
            .map(|(idx, g)| LodGroupMember::new(PrimitiveId(idx as u32), idx as u8, g, g))
            .collect();
        let snapshots = [[5, 3, 2], [2, 4, 0], [0, 2, 0]]
            .into_iter()
            .enumerate()
            .map(|(level, counts)| {
                let error = [0.0, 0.2, 0.1][level];
                let parts = counts
                    .into_iter()
                    .enumerate()
                    .filter(|(_, count)| *count > 0)
                    .map(|(idx, count)| {
                        LodPartition::new(
                            PrimitiveId(idx as u32),
                            Lod::new(geometry(count), error).unwrap(),
                        )
                    })
                    .collect();
                GroupSnapshot::new(parts, SnapshotDiagnostics::default()).unwrap()
            })
            .collect();
        let group = LodGroup::new(
            members,
            LodMetric::GeometricAbsolute,
            snapshots,
            None,
            LodProvenance {
                producer: "test".into(),
                settings: String::new(),
                stop_reason: "test".into(),
            },
        )
        .unwrap();
        (mesh, group)
    }

    #[test]
    fn snapshots_allow_material_growth_and_absence_with_atomic_error() {
        let (mut mesh, group) = fixture();
        assert_eq!(
            group
                .snapshots()
                .iter()
                .map(GroupSnapshot::triangle_count)
                .collect::<Vec<_>>(),
            [10, 6, 2]
        );
        assert!(group.snapshots()[1].partition(PrimitiveId(2)).is_none());
        assert_eq!(group.selection_error(2), Some(0.2));
        assert_eq!(group.selection_error(3), None);
        mesh.set_lod_groups(vec![group.clone()].into_boxed_slice())
            .unwrap();
        let bytes = bincode::serde::encode_to_vec(&mesh, bincode::config::legacy()).unwrap();
        let (decoded, _): (Mesh, _) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
        assert_eq!(decoded.lod_groups(), &[group]);
    }

    #[test]
    fn mesh_envelope_includes_expanded_group_geometry_and_ignores_unused_vertices() {
        let (mut mesh, mut group) = fixture();
        let source = geometry(2);
        let mut bytes = source.vertex_data().to_vec();
        for v in bytes.chunks_exact_mut(12) {
            v[8..12].copy_from_slice(&2f32.to_ne_bytes());
        }
        bytes.extend([100f32; 3].into_iter().flat_map(f32::to_ne_bytes));
        let expanded =
            Geometry::new(&bytes, VertexType::POSITION, source.indices().clone()).unwrap();
        group.snapshots[1] = GroupSnapshot::new(
            vec![LodPartition::new(
                PrimitiveId(0),
                Lod::new(expanded, 2.).unwrap(),
            )]
            .into_boxed_slice(),
            SnapshotDiagnostics::default(),
        )
        .unwrap();
        group.snapshots = group.snapshots[..2].to_vec().into_boxed_slice();
        mesh.set_lod_groups(vec![group].into_boxed_slice()).unwrap();
        assert_eq!(mesh.bounds().unwrap().min(), [0.; 3]);
        assert_eq!(mesh.bounds().unwrap().max(), [1., 1., 2.]);
        assert!(Mesh::new(Vec::new(), None).unwrap().bounds().is_none());
    }

    #[test]
    fn snapshots_reject_nonfinite_reconstructed_attributes() {
        let source = super::super::test::grid(2, false);
        let mut bytes = source.vertex_data().to_vec();
        bytes[24..28].copy_from_slice(&f32::NAN.to_ne_bytes());
        let geometry =
            Geometry::new(&bytes, source.vertex_type(), source.indices().clone()).unwrap();
        let lod = Lod::new(geometry, 0.).unwrap();
        assert!(
            GroupSnapshot::new(
                vec![LodPartition::new(PrimitiveId(0), lod)].into_boxed_slice(),
                SnapshotDiagnostics::default()
            )
            .is_err()
        );
    }

    #[test]
    fn grouped_archive_rejects_stale_sources_counts_bounds_and_membership() {
        let (mut mesh, group) = fixture();
        let rejects = |group: &LodGroup| {
            let bytes = bincode::serde::encode_to_vec(group, bincode::config::legacy()).unwrap();
            assert!(
                bincode::serde::decode_from_slice::<LodGroup, _>(&bytes, bincode::config::legacy())
                    .is_err()
            );
        };
        let mut invalid = group.clone();
        invalid.snapshots[1].triangle_count += 1;
        rejects(&invalid);
        let mut invalid = group.clone();
        invalid.snapshots[1].bounds.max[0] = 10.;
        rejects(&invalid);
        let mut invalid = group.clone();
        invalid.snapshots[1].partitions[0].primitive = PrimitiveId(99);
        rejects(&invalid);
        let mut invalid = group.clone();
        invalid.preparation = Some(LodPreparation {
            error: 1.,
            voxel_cells: 1,
            working_triangles: 10,
        });
        rejects(&invalid);
        let mut invalid = group.clone();
        invalid.members[0].source_identity ^= 1;
        assert!(
            mesh.set_lod_groups(vec![invalid].into_boxed_slice())
                .is_err()
        );
        assert!(mesh.lod_groups().is_empty());
        assert!(
            mesh.set_lod_groups(vec![group.clone(), group.clone()].into_boxed_slice())
                .is_err()
        );
        mesh.primitives_mut()[0]
            .set_lods(
                vec![
                    super::super::LodSet::new(
                        LodMetric::SourceOnly,
                        vec![Lod::new(geometry(5), 0.).unwrap()].into_boxed_slice(),
                    )
                    .unwrap(),
                ]
                .into_boxed_slice(),
            )
            .unwrap();
        assert!(mesh.set_lod_groups(vec![group].into_boxed_slice()).is_err());
    }
}
