use {
    super::MeshAsset,
    crate::{
        index::IndexBuffer,
        mesh::{Geometry, Lod, LodMetric, LodProvenance, LodSet, VertexType},
    },
    anyhow::{Context, ensure},
    glam::{DVec3, vec3},
    meshopt::{
        SimplifyOptions, VertexDataAdapter, build_meshlets_flex, simplify_with_attributes_and_locks,
    },
    ordered_float::OrderedFloat,
    serde::{Deserialize, Serialize},
    std::collections::{BTreeMap, BTreeSet},
};

// One declaration keeps partial pack/mesh settings and fully resolved defaults in sync.
macro_rules! settings {
    ($($name:ident: $ty:ty = $default:expr),* $(,)?) => {
        #[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq)]
        #[serde(rename_all = "kebab-case", deny_unknown_fields)]
        pub struct LodSettings { $(pub $name: Option<$ty>,)* }

        #[derive(Clone, Debug, Deserialize, Serialize)]
        #[serde(rename_all = "kebab-case")]
        pub struct ResolvedLodSettings { $(pub $name: $ty,)* }

        impl LodSettings {
            pub fn resolve(&self, defaults: &Self) -> anyhow::Result<ResolvedLodSettings> {
                let res = ResolvedLodSettings {
                    $($name: self.$name.or(defaults.$name).unwrap_or($default),)*
                };
                res.validate()?;
                Ok(res)
            }

            pub(super) fn inherit(&mut self, defaults: &Self) {
                $(self.$name = self.$name.or(defaults.$name);)*
            }
        }
    };
}

settings! {
    min_triangles: usize = MeshAsset::DEFAULT_LOD_MIN,
    target_ratio: OrderedFloat<f32> = OrderedFloat(0.5),
    target_error: OrderedFloat<f32> = OrderedFloat(MeshAsset::DEFAULT_LOD_TARGET_ERROR),
    cluster_vertices: usize = 128,
    cluster_triangles: usize = 128,
    spatial_split_factor: OrderedFloat<f32> = OrderedFloat(2.0),
    orientation_weight: OrderedFloat<f32> = OrderedFloat(0.0),
    group_size: usize = 4,
    max_group_size: usize = 32,
    proximity_weight: OrderedFloat<f32> = OrderedFloat(1.0),
    alignment_weight: OrderedFloat<f32> = OrderedFloat(0.0),
    normal_weight: OrderedFloat<f32> = OrderedFloat(0.5),
    permissive_retry: bool = true,
    lock_border: bool = false,
    min_reduction: OrderedFloat<f32> = OrderedFloat(0.01),
    iteration_limit: usize = 32,
    regrouping_retries: usize = 3,
}

impl ResolvedLodSettings {
    fn validate(&self) -> anyhow::Result<()> {
        ensure!(self.min_triangles > 0, "lod min-triangles must be positive");
        ensure!(
            (3..=256).contains(&self.cluster_vertices),
            "lod cluster-vertices must be 3..256"
        );
        ensure!(
            (4..=512).contains(&self.cluster_triangles) && self.cluster_triangles.is_multiple_of(4),
            "lod cluster-triangles must be 4..512 and divisible by four"
        );
        ensure!(
            self.group_size > 0
                && self.group_size <= self.max_group_size
                && self.max_group_size <= 1024,
            "lod group sizes must be 1..1024 with group-size <= max-group-size"
        );
        ensure!(
            (1..=1024).contains(&self.iteration_limit) && self.regrouping_retries <= 10,
            "lod search limits exceed supported range"
        );
        for (name, value) in [
            ("target-error", self.target_error.0),
            ("spatial-split-factor", self.spatial_split_factor.0),
            ("proximity-weight", self.proximity_weight.0),
            ("alignment-weight", self.alignment_weight.0),
            ("normal-weight", self.normal_weight.0),
        ] {
            ensure!(
                value.is_finite() && value >= 0.0,
                "lod {name} must be finite and nonnegative"
            );
        }
        ensure!(
            self.orientation_weight.0.is_finite()
                && (-1.0..=1.0).contains(&self.orientation_weight.0),
            "lod orientation-weight must be -1..1"
        );
        for (name, value) in [
            ("target-ratio", self.target_ratio.0),
            ("min-reduction", self.min_reduction.0),
        ] {
            ensure!(
                value.is_finite() && value > 0.0 && value < 1.0,
                "lod {name} must be strictly between zero and one"
            );
        }
        Ok(())
    }
}

pub(super) type Position = [u32; 3];

pub(super) fn position_key(position: DVec3) -> Position {
    position
        .to_array()
        .map(|v| if v == 0.0 { 0 } else { (v as f32).to_bits() })
}

/// A geometry operation, not a rendering role. Callers supply the requested layout's bytes.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct LodRequest {
    pub layout: VertexType,
    pub simplify: bool,
}

impl LodRequest {
    pub fn process(
        &self,
        source: Geometry,
        settings: &ResolvedLodSettings,
        locked_positions: &[[f32; 3]],
    ) -> anyhow::Result<LodSet> {
        ensure!(
            source.vertex_type() == self.layout,
            "lod request and input layouts differ"
        );
        settings.validate()?;
        let mut locks = BTreeSet::new();
        for position in locked_positions {
            let position = DVec3::from_array(position.map(f64::from));
            ensure!(position.is_finite(), "lod lock position must be finite");
            locks.insert(position_key(position));
        }
        let (levels, stop, metric) = if self.simplify {
            ensure!(
                matches!(
                    self.layout,
                    VertexType::POSITION | VertexType::PACKED_NORMAL
                ),
                "reduction unsupported for requested vertex layout"
            );
            let metric =
                if self.layout == VertexType::PACKED_NORMAL && settings.normal_weight.0 > 0.0 {
                    LodMetric::AttributeWeightedAbsolute
                } else {
                    LodMetric::GeometricAbsolute
                };
            let (levels, stop) = clustered_lods(source, &locks, settings)?;
            (levels, stop, metric)
        } else {
            (
                vec![Lod::new(source, 0.0)?].into_boxed_slice(),
                "source-only",
                LodMetric::SourceOnly,
            )
        };
        Ok(LodSet::new(metric, levels)?.with_provenance(LodProvenance {
            producer: crate::buf::MESH_LOD_PRODUCER.to_owned(),
            settings: toml::to_string(settings)?,
            stop_reason: stop.to_owned(),
        }))
    }
}

#[derive(Clone)]
struct Cluster {
    indices: Vec<u32>,
    error: f64,
    positions: BTreeSet<Position>,
    center: DVec3,
    normal: DVec3,
}

pub(super) fn rounded_error(error: f64) -> anyhow::Result<f32> {
    ensure!(
        error.is_finite() && error >= 0.0,
        "invalid accumulated lod error"
    );
    let mut rounded = error as f32;
    if (rounded as f64) < error {
        rounded = rounded.next_up();
    }
    ensure!(
        rounded.is_finite(),
        "accumulated lod error exceeds finite range"
    );
    Ok(rounded)
}

// Rebuild across ALL replaced and carried groups. Triangle provenance survives meshlet reorder
// (including cyclic corner rotations), so new clusters inherit only their actual input errors.
fn recluster(
    source: &Geometry,
    chunks: &[(Vec<u32>, f64)],
    settings: &ResolvedLodSettings,
) -> anyhow::Result<Vec<Cluster>> {
    let mut errors = BTreeMap::<[u32; 3], f64>::new();
    let mut indices = Vec::new();
    for (chunk, error) in chunks {
        indices.extend_from_slice(chunk);
        for triangle in chunk.chunks_exact(3) {
            let mut key: [u32; 3] = triangle.try_into().unwrap();
            key.sort_unstable();
            errors
                .entry(key)
                .and_modify(|e| *e = e.max(*error))
                .or_insert(*error);
        }
    }
    let vertices = VertexDataAdapter::new(source.vertex_data(), source.vertex_type().stride(), 0)?;
    let meshlets = build_meshlets_flex(
        &indices,
        &vertices,
        settings.cluster_vertices,
        (settings.cluster_triangles / 2 / 4 * 4).max(4),
        settings.cluster_triangles,
        settings.orientation_weight.0,
        settings.spatial_split_factor.0,
    );
    Ok(meshlets
        .iter()
        .map(|meshlet| {
            let indices = meshlet
                .triangles
                .iter()
                .map(|&idx| meshlet.vertices[idx as usize])
                .collect::<Vec<_>>();
            let positions = indices
                .iter()
                .map(|&idx| position_key(source.position(idx)))
                .collect::<BTreeSet<_>>();
            let center = positions
                .iter()
                .map(|p| DVec3::from_array(p.map(|v| f32::from_bits(v) as f64)))
                .sum::<DVec3>()
                / positions.len() as f64;
            let mut error = 0.0_f64;
            let mut normal = DVec3::ZERO;
            for triangle in indices.chunks_exact(3) {
                let a = source.position(triangle[0]);
                normal +=
                    (source.position(triangle[1]) - a).cross(source.position(triangle[2]) - a);
                let mut key: [u32; 3] = triangle.try_into().unwrap();
                key.sort_unstable();
                error = error.max(errors[&key]);
            }
            Cluster {
                indices,
                error,
                positions,
                center,
                normal: normal.normalize_or_zero(),
            }
        })
        .collect())
}

fn groups(
    clusters: &[Cluster],
    size: usize,
    settings: &ResolvedLodSettings,
    scale: f64,
) -> Vec<Vec<usize>> {
    const FRONTIER_LIMIT: usize = 64;
    const NEIGHBOR_WINDOW: usize = 8;

    let mut owners = BTreeMap::<Position, BTreeSet<usize>>::new();
    let mut spatial: [BTreeSet<(OrderedFloat<f64>, usize)>; 3] =
        std::array::from_fn(|_| BTreeSet::new());
    for (idx, cluster) in clusters.iter().enumerate() {
        for &position in &cluster.positions {
            owners.entry(position).or_default().insert(idx);
        }
        for axis in 0..3 {
            spatial[axis].insert((OrderedFloat(cluster.center[axis]), idx));
        }
    }
    let mut unused = (0..clusters.len()).collect::<BTreeSet<_>>();
    let mut groups = Vec::new();
    while let Some(seed) = unused.pop_first() {
        let mut group = Vec::new();
        let mut positions = BTreeSet::new();
        let mut center = DVec3::ZERO;
        let mut normal = DVec3::ZERO;
        let mut frontier = BTreeSet::new();
        let mut current = seed;
        loop {
            unused.remove(&current);
            for axis in 0..3 {
                spatial[axis].remove(&(OrderedFloat(clusters[current].center[axis]), current));
            }
            for &position in &clusters[current].positions {
                let adjacent = owners.get_mut(&position).unwrap();
                adjacent.remove(&current);
                if positions.insert(position) {
                    // Bound even pathological fans sharing one position. Membership is removed
                    // as clusters are consumed, so these queries never scan assigned clusters.
                    frontier.extend(
                        adjacent
                            .range(..current)
                            .rev()
                            .take(NEIGHBOR_WINDOW)
                            .copied(),
                    );
                    frontier.extend(adjacent.range(current..).take(NEIGHBOR_WINDOW).copied());
                }
            }
            frontier.remove(&current);
            center += clusters[current].center;
            normal += clusters[current].normal;
            group.push(current);
            if group.len() >= size || unused.is_empty() {
                break;
            }
            let centroid = center / group.len() as f64;
            let axis = normal.normalize_or_zero();
            if frontier.is_empty() {
                // A bounded local candidate set for disconnected components, not an exhaustive
                // nearest-neighbor scan. Three axes avoid privileging the mesh's orientation.
                for axis in 0..3 {
                    let pivot = (OrderedFloat(centroid[axis]), 0);
                    frontier.extend(
                        spatial[axis]
                            .range(..pivot)
                            .rev()
                            .take(NEIGHBOR_WINDOW)
                            .map(|&(_, idx)| idx),
                    );
                    frontier.extend(
                        spatial[axis]
                            .range(pivot..)
                            .take(NEIGHBOR_WINDOW)
                            .map(|&(_, idx)| idx),
                    );
                }
            }
            let score = |idx: usize| {
                let cluster = &clusters[idx];
                let shared = cluster.positions.intersection(&positions).count() as f64;
                let proximity =
                    1.0 / (1.0 + cluster.center.distance(centroid) / scale.max(f64::MIN_POSITIVE));
                shared
                    + settings.proximity_weight.0 as f64 * proximity
                    + settings.alignment_weight.0 as f64 * axis.dot(cluster.normal)
            };
            let mut ranked = frontier
                .iter()
                .map(|&idx| (score(idx), idx))
                .collect::<Vec<_>>();
            ranked.sort_unstable_by(|a, b| b.0.total_cmp(&a.0).then(a.1.cmp(&b.1)));
            current = ranked[0].1;
            // Keep exact boundary/proximity/alignment scoring, but only for a bounded frontier
            // plus new local neighbors. Lowest cluster index wins equal scores deterministically.
            frontier = ranked
                .into_iter()
                .skip(1)
                .take(FRONTIER_LIMIT - 1)
                .map(|(_, idx)| idx)
                .collect();
        }
        groups.push(group);
    }
    groups
}

fn group_locks(
    clusters: &[Cluster],
    groups: &[Vec<usize>],
    positions: &[Position],
    seams: &BTreeSet<Position>,
) -> Vec<bool> {
    let mut owners = BTreeMap::<Position, usize>::new();
    let mut locks = seams.clone();
    for (group_idx, group) in groups.iter().enumerate() {
        for &idx in group {
            for &position in &clusters[idx].positions {
                if owners
                    .insert(position, group_idx)
                    .is_some_and(|owner| owner != group_idx)
                {
                    locks.insert(position);
                }
            }
        }
    }
    positions.iter().map(|p| locks.contains(p)).collect()
}

impl MeshAsset {
    pub fn resolved_lod_settings(&self) -> anyhow::Result<ResolvedLodSettings> {
        let mut settings = self.lod.clone();
        // Shipped flat artist controls have final precedence over nested defaults/overrides.
        settings.min_triangles = self.min_lod_triangles.or(settings.min_triangles);
        settings.target_error = self.lod_target_error.or(settings.target_error);
        settings.lock_border = self.lod_lock_border.or(settings.lock_border);
        settings.resolve(&LodSettings::default())
    }

    #[cfg(test)]
    pub(super) fn clustered_lods(
        &self,
        source: Geometry,
        seams: &BTreeSet<Position>,
    ) -> anyhow::Result<(Box<[Lod]>, &'static str)> {
        let settings = self.resolved_lod_settings()?;
        clustered_lods(source, seams, &settings)
    }
}

fn clustered_lods(
    source: Geometry,
    seams: &BTreeSet<Position>,
    settings: &ResolvedLodSettings,
) -> anyhow::Result<(Box<[Lod]>, &'static str)> {
    let vertex_buf = source.vertex_data();
    let stride = source.vertex_type().stride();
    let vertices = VertexDataAdapter::new(vertex_buf, stride, 0)
        .context("creating clustered lod vertex adapter")?;
    let indices = source.indices().as_u32();
    let mut min = DVec3::splat(f64::INFINITY);
    let mut max = DVec3::splat(f64::NEG_INFINITY);
    for &index in &indices {
        let position = source.position(index);
        min = min.min(position);
        max = max.max(position);
    }

    // Match meshoptimizer's maximum-axis extent without letting unused vertices
    // enlarge the original error budget or change spatial grouping scores.
    let scale = (max - min).max_element() as f32;
    ensure!(
        scale.is_finite() && scale >= 0.0,
        "invalid simplifier error scale"
    );
    let budget = settings.target_error.0 as f64 * scale as f64;
    rounded_error(budget)?;
    let positions = (0..source.vertex_count() as u32)
        .map(|idx| position_key(source.position(idx)))
        .collect::<Vec<_>>();
    let normals = if source.vertex_type() == VertexType::PACKED_NORMAL {
        vertex_buf
            .chunks_exact(stride)
            .flat_map(|vertex| {
                let x = i16::from_ne_bytes(vertex[12..14].try_into().unwrap()) as f32 / 32767.0;
                let y = i16::from_ne_bytes(vertex[14..16].try_into().unwrap()) as f32 / 32767.0;
                let mut normal = vec3(x, y, 1.0 - x.abs() - y.abs());
                let t = (-normal.z).max(0.0);
                normal.x += if x >= 0.0 { -t } else { t };
                normal.y += if y >= 0.0 { -t } else { t };
                normal.normalize().to_array()
            })
            .collect::<Vec<_>>()
    } else {
        Vec::new()
    };
    let weights: &[f32] = if normals.is_empty() {
        &[]
    } else {
        &[settings.normal_weight.0; 3]
    };
    // Ignore vertices outside each group's index subset, including unused normal variants.
    let mut opts = SimplifyOptions::Sparse | SimplifyOptions::ErrorAbsolute;
    if settings.lock_border {
        opts |= SimplifyOptions::LockBorder;
    }
    let mut lods = vec![Lod::new(source.clone(), 0.0)?];
    let mut chunks = vec![(indices, 0.0)];
    let mut count = chunks[0].0.len() / 3;
    let mut stop = "iteration-limit";
    for _ in 0..settings.iteration_limit {
        if count <= settings.min_triangles {
            stop = "triangle-floor";
            break;
        }
        let clusters = recluster(&source, &chunks, &settings)?;
        let mut accepted = None;
        let mut available_budget = false;
        for retry in 0..=settings.regrouping_retries {
            let size = (settings.group_size << retry).min(settings.max_group_size);
            let groups = groups(&clusters, size, &settings, scale as f64);
            let locks = group_locks(&clusters, &groups, &positions, seams);
            let mut next = Vec::new();
            let mut next_count = count;
            for group in groups {
                let inherited = group
                    .iter()
                    .map(|&idx| clusters[idx].error)
                    .fold(0.0_f64, f64::max);
                let remaining = (budget - inherited).max(0.0);
                let mut limit = remaining as f32;
                if limit as f64 > remaining {
                    limit = limit.next_down();
                }
                available_budget |= limit > 0.0;
                let indices = group
                    .iter()
                    .flat_map(|&idx| clusters[idx].indices.iter().copied())
                    .collect::<Vec<_>>();
                let group_count = indices.len() / 3;
                let floor = group_count
                    .saturating_sub(next_count - settings.min_triangles)
                    .max(1);
                let target =
                    ((group_count as f64 * settings.target_ratio.0 as f64) as usize).max(floor);
                let mut replacement = None;
                if limit > 0.0 && target < group_count {
                    for permissive in [false, true] {
                        if permissive && !settings.permissive_retry {
                            break;
                        }
                        let mut error = 0.0;
                        let reduced = simplify_with_attributes_and_locks(
                            &indices,
                            &vertices,
                            &normals,
                            weights,
                            if normals.is_empty() { 0 } else { 12 },
                            &locks,
                            target * 3,
                            limit,
                            opts | if permissive {
                                SimplifyOptions::Permissive
                            } else {
                                SimplifyOptions::None
                            },
                            Some(&mut error),
                        );
                        ensure!(
                            error.is_finite() && error >= 0.0,
                            "invalid absolute simplification error"
                        );
                        let reduced_count = reduced.len() / 3;
                        // Outward f64 accumulation also covers widely separated exponents.
                        // Do not rescale an ErrorAbsolute result by the source scale again.
                        let accumulated = if inherited > 0.0 && error > 0.0 {
                            (inherited + error as f64).next_up()
                        } else {
                            inherited + error as f64
                        };
                        if reduced_count >= floor
                            && reduced_count < group_count
                            && (group_count - reduced_count) as f64 / group_count as f64
                                >= settings.min_reduction.0 as f64
                            && accumulated <= budget
                        {
                            // Source geometry already validates the immutable vertex buffer.
                            // Only replacement indices need checking for each reduced group.
                            ensure!(
                                reduced.len() >= 3 && reduced.len().is_multiple_of(3),
                                "geometry indices must be triangles"
                            );
                            let vertex_count = source.vertex_count();
                            ensure!(
                                reduced.iter().all(|&index| (index as usize) < vertex_count),
                                "geometry index exceeds vertex count"
                            );
                            replacement = Some((reduced, accumulated));
                            break;
                        }
                    }
                }
                if let Some((indices, error)) = replacement {
                    next_count -= group_count - indices.len() / 3;
                    next.push((indices, error));
                } else {
                    // A stalled cluster retains its own error, not its group's maximum.
                    next.extend(
                        group
                            .iter()
                            .map(|&idx| (clusters[idx].indices.clone(), clusters[idx].error)),
                    );
                }
            }
            if next_count < count {
                if next_count < accepted.as_ref().map(|(_, best)| *best).unwrap_or(count) {
                    accepted = Some((next, next_count));
                }
                // A tiny successful region must not prevent stalled regions from getting
                // larger groups. Keep the best valid candidate if all retries remain weak.
                if next_count <= settings.min_triangles
                    || (count - next_count) as f64 / count as f64 >= settings.min_reduction.0 as f64
                {
                    break;
                }
            }
            if size == settings.max_group_size || size >= clusters.len() {
                break;
            }
        }
        let Some((next, next_count)) = accepted else {
            stop = if available_budget {
                "stalled-after-regrouping"
            } else {
                "error-budget-exhausted"
            };
            break;
        };
        chunks = next;
        count = next_count;
        let error = rounded_error(
            chunks
                .iter()
                .map(|(_, error)| *error)
                .fold(0.0_f64, f64::max),
        )?;
        let mut indices = chunks
            .iter()
            .flat_map(|(indices, _)| indices.iter().copied())
            .collect::<Vec<_>>();
        let (compact, _) = MeshAsset::compact_mesh_indices(&mut indices, vertex_buf, stride)?;
        lods.push(Lod::new(
            Geometry::new(&compact, source.vertex_type(), IndexBuffer::new(&indices)?)?,
            error,
        )?);
        if count <= settings.min_triangles {
            stop = "triangle-floor";
            break;
        }
    }
    Ok((lods.into_boxed_slice(), stop))
}

#[cfg(test)]
mod test {
    use {super::*, crate::mesh::test::grid, meshopt::simplify_scale};

    #[test]
    fn settings_inherit_per_field_and_flat_artist_controls_win() {
        let defaults: LodSettings = toml::from_str("min-triangles = 77\ntarget-error = 0.2\ngroup-size = 8\nalignment-weight = 2.0\nlock-border = true").unwrap();
        let mut mesh: MeshAsset = toml::from_str("min-lod-triangles = 9\nlod-target-error = 0.01\nlod-lock-border = false\n[lod]\nmin-triangles = 33\ngroup-size = 2").unwrap();
        mesh.lod.inherit(&defaults);
        let settings = mesh.resolved_lod_settings().unwrap();
        assert_eq!(settings.min_triangles, 9);
        assert_eq!(settings.target_error.0, 0.01);
        assert!(!settings.lock_border);
        assert_eq!(settings.group_size, 2);
        assert_eq!(settings.alignment_weight.0, 2.0);
        assert_eq!(settings.cluster_vertices, 128);
        let encoded = toml::to_string(&settings).unwrap();
        let decoded: LodSettings = toml::from_str(&encoded).unwrap();
        assert_eq!(
            encoded,
            toml::to_string(&decoded.resolve(&LodSettings::default()).unwrap()).unwrap()
        );
    }

    #[test]
    fn settings_reject_invalid_ranges_before_native_calls() {
        for invalid in [
            "min-triangles = 0",
            "cluster-vertices = 2",
            "cluster-vertices = 257",
            "cluster-triangles = 3",
            "cluster-triangles = 6",
            "cluster-triangles = 516",
            "target-ratio = 0.0",
            "target-ratio = 1.0",
            "min-reduction = nan",
            "target-error = inf",
            "normal-weight = -1.0",
            "proximity-weight = nan",
            "alignment-weight = -0.1",
            "spatial-split-factor = -1.0",
            "orientation-weight = 2.0",
            "group-size = 0",
            "group-size = 33",
            "max-group-size = 1025",
            "iteration-limit = 0",
            "iteration-limit = 1025",
            "regrouping-retries = 11",
        ] {
            let settings: LodSettings = toml::from_str(invalid).unwrap();
            assert!(
                settings.resolve(&LodSettings::default()).is_err(),
                "accepted {invalid}"
            );
        }
        assert!(toml::from_str::<LodSettings>("misspelled = 1").is_err());
    }

    fn cluster(positions: &[Position], center: DVec3, normal: DVec3) -> Cluster {
        Cluster {
            indices: vec![],
            error: 0.0,
            positions: positions.iter().copied().collect(),
            center,
            normal,
        }
    }

    #[test]
    fn positional_locks_cover_every_split_variant_and_stalled_neighbor() {
        let a = position_key(DVec3::ZERO);
        let b = position_key(DVec3::X);
        let c = position_key(DVec3::Y);
        let clusters = vec![
            cluster(&[a, b], DVec3::ZERO, DVec3::Z),
            cluster(&[b, c], DVec3::ZERO, DVec3::Z),
        ];
        let positions = [a, b, b, c]; // Two immutable normal variants at b.
        assert_eq!(
            group_locks(&clusters, &[vec![0], vec![1]], &positions, &BTreeSet::new()),
            [false, true, true, false]
        );
        assert_eq!(
            group_locks(&clusters, &[vec![0, 1]], &positions, &BTreeSet::from([c])),
            [false, false, false, true]
        );
        assert_eq!(position_key(DVec3::new(-0.0, 0.0, -0.0)), a);
    }

    #[test]
    fn proximity_and_alignment_change_group_selection_with_stable_ties() {
        let clusters = vec![
            cluster(&[], DVec3::ZERO, DVec3::Z),
            cluster(&[], DVec3::X, -DVec3::Z),
            cluster(&[], DVec3::X * 10.0, DVec3::Z),
        ];
        let mut settings = LodSettings::default()
            .resolve(&LodSettings::default())
            .unwrap();
        assert_eq!(groups(&clusters, 2, &settings, 1.0)[0], [0, 1]);
        settings.alignment_weight = OrderedFloat(1.0);
        assert_eq!(groups(&clusters, 2, &settings, 1.0)[0], [0, 2]);
        settings.proximity_weight = OrderedFloat(0.0);
        settings.alignment_weight = OrderedFloat(0.0);
        assert_eq!(groups(&clusters, 2, &settings, 1.0)[0], [0, 1]);
    }

    #[test]
    fn reclustering_inherits_actual_triangle_errors_without_rescaling() {
        let source = grid(9, true);
        let indices = source.indices().as_u32();
        let chunks = vec![
            (indices[..3].to_vec(), 0.125),
            (indices[3..].to_vec(), 0.03125),
        ];
        let settings: LodSettings =
            toml::from_str("cluster-vertices = 3\ncluster-triangles = 4").unwrap();
        let settings = settings.resolve(&LodSettings::default()).unwrap();
        let clusters = recluster(&source, &chunks, &settings).unwrap();
        assert_eq!(
            clusters.iter().map(|c| c.indices.len()).sum::<usize>(),
            indices.len()
        );
        assert_eq!(clusters.iter().filter(|c| c.error == 0.125).count(), 1);
        assert!(clusters.iter().filter(|c| c.error == 0.03125).count() > 1);
        let inherited = clusters.iter().map(|c| c.error).fold(0.0_f64, f64::max);
        assert_eq!(rounded_error(inherited + 0.0625).unwrap(), 0.1875);
    }

    #[test]
    fn larger_group_retries_unlock_stalls_and_iterations_repartition() {
        let source = grid(17, true);
        let recipe = "min-lod-triangles = 8\nlod-target-error = 0.1\n[lod]\ncluster-vertices = 3\ncluster-triangles = 4\ngroup-size = 1\nmax-group-size = 32";
        let small: MeshAsset =
            toml::from_str(&format!("{recipe}\nregrouping-retries = 0")).unwrap();
        let retry: MeshAsset =
            toml::from_str(&format!("{recipe}\nregrouping-retries = 5")).unwrap();
        let (stalled, stop) = small
            .clustered_lods(source.clone(), &BTreeSet::new())
            .unwrap();
        assert_eq!(stalled.len(), 1);
        assert_eq!(stop, "stalled-after-regrouping");
        let (reduced, _) = retry
            .clustered_lods(source.clone(), &BTreeSet::new())
            .unwrap();
        assert!(
            reduced.len() > 2,
            "retries must enable multiple actual replacement passes"
        );
        let single: MeshAsset = toml::from_str(&format!(
            "{recipe}\nregrouping-retries = 5\niteration-limit = 1"
        ))
        .unwrap();
        let (first, stop) = single.clustered_lods(source, &BTreeSet::new()).unwrap();
        assert_eq!(first.len(), 2);
        assert_eq!(stop, "iteration-limit");
        assert_eq!(&reduced[..2], &*first);
    }

    #[test]
    fn exhausted_budget_and_material_seams_never_discard_geometry() {
        let source = grid(9, true);
        let zero: MeshAsset =
            toml::from_str("min-lod-triangles = 1\nlod-target-error = 0.0").unwrap();
        let (lods, stop) = zero
            .clustered_lods(source.clone(), &BTreeSet::new())
            .unwrap();
        assert_eq!(lods.len(), 1);
        assert_eq!(stop, "error-budget-exhausted");
        let mesh: MeshAsset =
            toml::from_str("min-lod-triangles = 1\nlod-target-error = 0.1").unwrap();
        let seams = (0..source.vertex_count() as u32)
            .map(|idx| position_key(source.position(idx)))
            .collect();
        let (lods, stop) = mesh.clustered_lods(source, &seams).unwrap();
        assert_eq!(lods.len(), 1);
        assert_eq!(stop, "stalled-after-regrouping");
    }

    #[test]
    fn successful_groups_preserve_adjacent_locked_stalled_triangles_and_edges() {
        let source = grid(17, true);
        let seams = (0..source.vertex_count() as u32)
            .filter(|&idx| source.position(idx).x <= 8.0)
            .map(|idx| position_key(source.position(idx)))
            .collect();
        let asset: MeshAsset = toml::from_str("min-lod-triangles = 8\nlod-target-error = 0.1\n[lod]\ncluster-vertices = 16\ncluster-triangles = 16\ngroup-size = 2").unwrap();
        let (lods, _) = asset.clustered_lods(source.clone(), &seams).unwrap();
        assert!(lods.len() > 2);
        let locked = crate::mesh::test::triangles(&source, false)
            .into_iter()
            .filter(|t| t.iter().all(|p| f32::from_bits(p[0]) <= 8.0))
            .collect::<Vec<_>>();
        for lod in &lods[1..] {
            let triangles = crate::mesh::test::triangles(lod.geometry(), false);
            assert!(locked.iter().all(|t| triangles.binary_search(t).is_ok()));
            // Every positional edge remains paired, including the successful/stalled interface.
            let mut edges = BTreeMap::new();
            for t in triangles {
                for [a, b] in [[t[0], t[1]], [t[1], t[2]], [t[2], t[0]]] {
                    let edge = if a < b { [a, b] } else { [b, a] };
                    *edges.entry(edge).or_insert(0) += 1;
                }
            }
            for (edge, count) in edges {
                if count == 1 {
                    assert!(
                        (0..2).any(|axis| {
                            let a = f32::from_bits(edge[0][axis]);
                            let b = f32::from_bits(edge[1][axis]);
                            (a == 0.0 && b == 0.0) || (a == 16.0 && b == 16.0)
                        }),
                        "unexpected interior boundary: {edge:?}"
                    );
                } else {
                    assert_eq!(count, 2);
                }
            }
        }
    }

    #[test]
    fn single_group_snapshots_store_the_sum_of_actual_absolute_native_errors() {
        let source = grid(9, true);
        let asset: MeshAsset = toml::from_str("min-lod-triangles = 8\nlod-target-error = 1.0\n[lod]\ncluster-vertices = 256\ncluster-triangles = 512\ngroup-size = 1\npermissive-retry = false").unwrap();
        let settings = asset.resolved_lod_settings().unwrap();
        let (lods, _) = asset
            .clustered_lods(source.clone(), &BTreeSet::new())
            .unwrap();
        assert!(lods.len() >= 3);
        let adapter =
            VertexDataAdapter::new(source.vertex_data(), source.vertex_type().stride(), 0).unwrap();
        let locks = vec![false; source.vertex_count()];
        let budget = simplify_scale(&adapter) as f64;
        let mut chunks = vec![(source.indices().as_u32(), 0.0)];
        let mut accumulated = 0.0_f64;
        for (level, lod) in lods.iter().enumerate().skip(1) {
            let clusters = recluster(&source, &chunks, &settings).unwrap();
            assert_eq!(clusters.len(), 1);
            assert_eq!(clusters[0].error, accumulated);
            let indices = &clusters[0].indices;
            let remaining = budget - accumulated;
            let mut limit = remaining as f32;
            if limit as f64 > remaining {
                limit = limit.next_down();
            }
            let mut error = 0.0;
            let reduced = simplify_with_attributes_and_locks(
                indices,
                &adapter,
                &[],
                &[],
                0,
                &locks,
                (indices.len() / 3 / 2).max(8) * 3,
                limit,
                SimplifyOptions::Sparse | SimplifyOptions::ErrorAbsolute,
                Some(&mut error),
            );
            assert_eq!(reduced.len(), lod.geometry().indices().index_count());
            accumulated = if accumulated > 0.0 && error > 0.0 {
                (accumulated + error as f64).next_up()
            } else {
                accumulated + error as f64
            };
            assert_eq!(lod.error(), rounded_error(accumulated).unwrap());
            if level > 1 {
                assert!(
                    lod.error() > error,
                    "snapshot must include inherited error, not just the last replacement"
                );
            }
            chunks = vec![(reduced, accumulated)];
        }
    }

    #[test]
    fn sparse_subset_ignores_unused_locked_normal_variants_and_distant_vertices() {
        let source = grid(9, true);
        let stride = source.vertex_type().stride();
        let indices = source.indices().as_u32()[..192].to_vec();
        let mut compact_indices = indices.clone();
        let (compact, _) =
            MeshAsset::compact_mesh_indices(&mut compact_indices, source.vertex_data(), stride)
                .unwrap();
        let attributes = |vertices: &[u8]| {
            vertices
                .chunks_exact(stride)
                .flat_map(|vertex| {
                    vertex[12..24]
                        .chunks_exact(4)
                        .map(|value| f32::from_ne_bytes(value.try_into().unwrap()))
                })
                .collect::<Vec<_>>()
        };
        let mut padded = source.vertex_data().to_vec();
        for vertex in source.vertex_data().chunks_exact(stride) {
            let mut variant = vertex.to_vec();
            for axis in 0..3 {
                let offset = 12 + axis * 4;
                let value = f32::from_ne_bytes(variant[offset..offset + 4].try_into().unwrap());
                variant[offset..offset + 4].copy_from_slice(&(-value).to_ne_bytes());
            }
            padded.extend(variant);
        }
        let mut distant = source.vertex_data()[..stride].to_vec();
        distant[..4].copy_from_slice(&10000.0_f32.to_ne_bytes());
        padded.extend(distant);
        let compact_adapter = VertexDataAdapter::new(&compact, stride, 0).unwrap();
        let padded_adapter = VertexDataAdapter::new(&padded, stride, 0).unwrap();
        let locks = |vertices: &[u8]| {
            vertices
                .chunks_exact(stride)
                .map(|vertex| f32::from_ne_bytes(vertex[..4].try_into().unwrap()) == 0.0)
                .collect::<Vec<_>>()
        };
        let mut padded_locks = locks(&padded);
        padded_locks[source.vertex_count()..].fill(true);
        for options in [
            SimplifyOptions::None,
            SimplifyOptions::Permissive,
            SimplifyOptions::Permissive | SimplifyOptions::LockBorder,
        ] {
            let options = options | SimplifyOptions::Sparse | SimplifyOptions::ErrorAbsolute;
            let mut compact_error = 0.0;
            let mut padded_error = 0.0;
            let expected = simplify_with_attributes_and_locks(
                &compact_indices,
                &compact_adapter,
                &attributes(&compact),
                &[0.5; 3],
                12,
                &locks(&compact),
                48,
                0.2,
                options,
                Some(&mut compact_error),
            );
            let actual = simplify_with_attributes_and_locks(
                &indices,
                &padded_adapter,
                &attributes(&padded),
                &[0.5; 3],
                12,
                &padded_locks,
                48,
                0.2,
                options,
                Some(&mut padded_error),
            );
            assert!(actual.len() < indices.len());
            assert_eq!(padded_error, compact_error);
            assert!(actual.iter().all(|idx| indices.contains(idx)));
            let expected = Geometry::new(
                &compact,
                source.vertex_type(),
                IndexBuffer::new(&expected).unwrap(),
            )
            .unwrap();
            let actual = Geometry::new(
                &padded,
                source.vertex_type(),
                IndexBuffer::new(&actual).unwrap(),
            )
            .unwrap();
            assert_eq!(
                crate::mesh::test::triangles(&actual, true),
                crate::mesh::test::triangles(&expected, true)
            );
        }
        // Exercise the producer as well as the native subset contract: unused packed-normal
        // variants must not turn the referenced smooth surface into a hard-seamed mesh.
        let compact = Geometry::new(
            &compact,
            source.vertex_type(),
            IndexBuffer::new(&compact_indices).unwrap(),
        )
        .unwrap();
        let guide = MeshAsset::new("unused.glb")
            .layout_lods(&compact, VertexType::PACKED_NORMAL)
            .unwrap();
        let compact = guide.levels()[0].geometry();
        let mut padded = compact.vertex_data().to_vec();
        for vertex in compact.vertex_data().chunks_exact(16) {
            padded.extend_from_slice(&vertex[..12]);
            padded.extend(0_i16.to_ne_bytes());
            padded.extend(i16::MAX.to_ne_bytes());
        }
        let padded = Geometry::new(
            &padded,
            VertexType::PACKED_NORMAL,
            compact.indices().clone(),
        )
        .unwrap();
        let asset: MeshAsset =
            toml::from_str("min-lod-triangles = 8\nlod-target-error = 0.05").unwrap();
        let expected = asset
            .clustered_lods(compact.clone(), &BTreeSet::new())
            .unwrap()
            .0;
        let actual = asset.clustered_lods(padded, &BTreeSet::new()).unwrap().0;
        assert!(expected.len() > 1);
        assert_eq!(actual.len(), expected.len());
        for (actual, expected) in actual.iter().zip(expected.iter()).skip(1) {
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn sparse_source_outliers_do_not_change_lod_budget_or_reductions() {
        for layout in [VertexType::POSITION, VertexType::PACKED_NORMAL] {
            let source = grid(17, true).project(layout).unwrap();
            let mut vertices = source.vertex_data().to_vec();
            let mut outlier = vertices[..layout.stride()].to_vec();
            outlier[..4].copy_from_slice(&100_000.0_f32.to_ne_bytes());
            vertices.extend(outlier);
            let padded = Geometry::new(&vertices, layout, source.indices().clone()).unwrap();
            let request = LodRequest {
                layout,
                simplify: true,
            };
            let mut settings = LodSettings::default()
                .resolve(&LodSettings::default())
                .unwrap();
            settings.min_triangles = 8;

            for target_error in [0.0001, 0.05] {
                settings.target_error = OrderedFloat(target_error);
                let expected = request.process(source.clone(), &settings, &[]).unwrap();
                let actual = request.process(padded.clone(), &settings, &[]).unwrap();
                assert_eq!(actual.levels()[0].geometry().vertex_data(), vertices);
                assert_eq!(
                    crate::mesh::test::triangles(actual.levels()[0].geometry(), false),
                    crate::mesh::test::triangles(&source, false)
                );
                assert_eq!(actual.levels().len(), expected.levels().len());
                assert_eq!(&actual.levels()[1..], &expected.levels()[1..]);
                assert_eq!(actual.provenance(), expected.provenance());

                if target_error == 0.0001 {
                    assert_eq!(expected.levels().len(), 1);
                } else {
                    assert!(expected.levels().len() > 1);
                }

                let budget = rounded_error(target_error as f64 * 16.0).unwrap();
                assert!(actual.levels().iter().all(|lod| lod.error() <= budget));
            }
        }
    }

    #[test]
    fn bounded_group_frontiers_partition_dense_fans_and_disconnected_components() {
        let settings = LodSettings::default()
            .resolve(&LodSettings::default())
            .unwrap();
        for connected in [false, true] {
            let clusters = (0..4096)
                .map(|idx| {
                    cluster(
                        if connected { &[[0; 3]] } else { &[] },
                        DVec3::new((idx % 16) as f64, (idx / 16) as f64, 0.0),
                        DVec3::Z,
                    )
                })
                .collect::<Vec<_>>();
            let partition = groups(&clusters, 4, &settings, 256.0);
            assert_eq!(partition.len(), 1024);
            let mut indices = partition.iter().flatten().copied().collect::<Vec<_>>();
            indices.sort_unstable();
            assert_eq!(indices, (0..4096).collect::<Vec<_>>());
            assert_eq!(partition, groups(&clusters, 4, &settings, 256.0));
        }
    }

    #[test]
    fn small_aggregate_progress_still_gets_larger_group_retries() {
        let source = grid(17, true);
        let recipe = "min-lod-triangles = 8\nlod-target-error = 0.1\n[lod]\ncluster-vertices = 16\ncluster-triangles = 16\ngroup-size = 1\nmax-group-size = 8\nmin-reduction = 0.2\niteration-limit = 1";
        let small: MeshAsset =
            toml::from_str(&format!("{recipe}\nregrouping-retries = 0")).unwrap();
        let retry: MeshAsset =
            toml::from_str(&format!("{recipe}\nregrouping-retries = 3")).unwrap();
        let seams = (0..source.vertex_count() as u32)
            .filter(|&idx| source.position(idx).x <= 12.0)
            .map(|idx| position_key(source.position(idx)))
            .collect();
        let (first, _) = small.clustered_lods(source.clone(), &seams).unwrap();
        let (retried, _) = retry.clustered_lods(source.clone(), &seams).unwrap();
        let original = source.indices().triangle_count();
        let first_count = first.last().unwrap().geometry().indices().triangle_count();
        assert!(
            first_count < original,
            "fixture must make small initial progress"
        );
        assert!((original - first_count) as f64 / (original as f64) < 0.2);
        assert!(
            retried
                .last()
                .unwrap()
                .geometry()
                .indices()
                .triangle_count()
                < first_count,
            "larger groups must improve on already successful small groups"
        );
    }
}
