use {
    super::{
        ResolvedLodSettings, clustered_lods, position_key, rounded_error,
        surface::{SAMPLES, Surface, Union, Vertex, interpolate},
        voxel,
    },
    crate::{
        index::IndexBuffer,
        mesh::{
            Geometry, GroupSnapshot, Lod, LodGroup, LodGroupMember, LodMetric, LodPartition,
            LodPreparation, LodProvenance, PrimitiveId, SnapshotDiagnostics, VertexType,
        },
    },
    anyhow::{Context as _, ensure},
    glam::DVec3,
    meshopt::{VertexDataAdapter, optimize_overdraw_in_place, optimize_vertex_cache_in_place},
    ordered_float::OrderedFloat,
    serde::Serialize,
    std::collections::{BTreeMap, BTreeSet},
};

/// Final buffer optimization, applied only after triangles have their final material owners.
#[derive(Clone, Copy, Debug, Serialize)]
#[serde(rename_all = "kebab-case")]
pub struct LodFinalizeSettings {
    pub optimize: bool,
    pub optimize_vertex_cache: bool,
    pub overdraw_threshold: f32,
    pub max_index: Option<u32>,
}

impl Default for LodFinalizeSettings {
    fn default() -> Self {
        Self {
            optimize: true,
            optimize_vertex_cache: true,
            overdraw_threshold: 1.05,
            max_index: None,
        }
    }
}

/// A neutral request for complete, material-partitioned snapshots of static geometry.
#[derive(Clone, Copy, Debug)]
pub struct LodGroupRequest {
    pub layout: VertexType,
    pub simplify: bool,
}

/// Canonical geometry and, optionally, an earlier unsimplified appearance reference.
#[derive(Clone, Copy, Debug)]
pub struct LodSource<'a> {
    pub primitive: PrimitiveId,
    pub material: u8,
    pub geometry: &'a Geometry,
    pub reference: Option<&'a Geometry>,
}

impl LodGroupRequest {
    pub fn process(
        &self,
        sources: &[LodSource<'_>],
        settings: &ResolvedLodSettings,
        locked_positions: &[[f32; 3]],
        finalize: &LodFinalizeSettings,
    ) -> anyhow::Result<LodGroup> {
        settings.validate()?;
        self.layout.validate()?;
        ensure!(
            !sources.is_empty() && sources.windows(2).all(|s| s[0].primitive < s[1].primitive),
            "group sources must be nonempty, unique and sorted"
        );
        ensure!(
            settings.join || sources.len() == 1,
            "multiple sources require join"
        );
        ensure!(
            !self.layout.contains(VertexType::JOINTS_WEIGHTS),
            "grouped lods require a static layout"
        );
        ensure!(
            finalize.overdraw_threshold.is_finite() && finalize.overdraw_threshold >= 1.0,
            "invalid finalize overdraw threshold"
        );
        ensure!(
            finalize.max_index.is_none_or(|v| v >= 2),
            "finalize max-index must allow three vertices"
        );
        let mut locks = BTreeSet::new();
        for position in locked_positions {
            let p = DVec3::from_array(position.map(f64::from));
            ensure!(p.is_finite(), "lod lock position must be finite");
            locks.insert(position_key(p));
        }
        let mut members = Vec::new();
        let mut parts = Vec::new();
        for source in sources {
            ensure!(
                !source
                    .geometry
                    .vertex_type()
                    .contains(VertexType::JOINTS_WEIGHTS),
                "group source is deformed"
            );
            let reference = source.reference.unwrap_or(source.geometry);
            ensure!(
                !reference.vertex_type().contains(VertexType::JOINTS_WEIGHTS),
                "group appearance reference is deformed"
            );
            // Also validate requested attributes before constructing any candidate.
            let projected = source.geometry.project(self.layout)?;
            ensure!(
                finalize.max_index.is_none_or(|max| projected
                    .indices()
                    .as_u32()
                    .into_iter()
                    .all(|idx| idx <= max)),
                "canonical source exceeds finalize max-index"
            );
            reference.project(self.layout)?;
            ensure!(
                !self.simplify
                    || !self.layout.contains(VertexType::TANGENT)
                    || reference.vertex_type().contains(VertexType::TEXTURE0),
                "tangent reconstruction requires source texture0"
            );
            members.push(LodGroupMember::new(
                source.primitive,
                source.material,
                source.geometry,
                reference,
            ));
            parts.push(LodPartition::new(
                source.primitive,
                Lod::new(projected, 0.0)?,
            ));
        }
        let mut snapshots = vec![GroupSnapshot::new(
            parts.into_boxed_slice(),
            SnapshotDiagnostics::default(),
        )?];
        #[derive(Serialize)]
        struct ProvenanceSettings<'a> {
            #[serde(flatten)]
            processing: &'a ResolvedLodSettings,
            finalize: &'a LodFinalizeSettings,
        }
        let assemble = |snapshots: Vec<GroupSnapshot>, metric, preparation, stop_reason: String| {
            LodGroup::new(
                members.clone().into_boxed_slice(),
                metric,
                snapshots.into_boxed_slice(),
                preparation,
                LodProvenance {
                    producer: crate::buf::MESH_LOD_PRODUCER.to_owned(),
                    settings: toml::to_string(&ProvenanceSettings {
                        processing: settings,
                        finalize,
                    })?,
                    stop_reason,
                },
            )
        };
        if !self.simplify {
            return assemble(snapshots, LodMetric::SourceOnly, None, "source-only".into());
        }
        let normal = self.layout.contains(VertexType::NORMAL)
            || self.layout.contains(VertexType::PACKED_NORMAL);
        let working_layout = if normal {
            VertexType::PACKED_NORMAL
        } else {
            VertexType::POSITION
        };
        let sampled = settings.voxelize || self.layout != working_layout;
        let weighted = normal && settings.normal_weight.0 > 0.0;
        let metric = match (sampled, weighted) {
            (false, false) => LodMetric::GeometricAbsolute,
            (false, true) => LodMetric::AttributeWeightedAbsolute,
            (true, false) => LodMetric::SampledGeometricAbsolute,
            (true, true) => LodMetric::SampledAttributeWeightedAbsolute,
        };
        let reference = Surface::new(
            sources
                .iter()
                .enumerate()
                .map(|(idx, s)| (idx, s.material, s.reference.unwrap_or(s.geometry).clone())),
            self.layout,
        )?;
        let canonical = Surface::new(
            sources
                .iter()
                .enumerate()
                .map(|(idx, s)| (idx, s.material, s.geometry.clone())),
            VertexType::POSITION,
        )?;
        let reference_offset = if sources.iter().any(|s| s.reference.is_some()) {
            canonical.deviation(&reference, 0.0)
        } else {
            0.0
        };
        let (min, max) = canonical.bounds();
        let scale = (max - min).max_element() as f32;
        let budget = settings.target_error.0 as f64 * scale as f64;
        rounded_error(budget)?;
        let projected = sources
            .iter()
            .map(|s| s.geometry.project(working_layout))
            .collect::<anyhow::Result<Vec<_>>>()?;
        let mut working = join(&projected, working_layout, settings.join)?;
        let mut cells = 0;
        if settings.voxelize {
            if !locks.is_empty() {
                return assemble(snapshots, metric, None, "voxel-external-locks".into());
            }
            let surface = Surface::new([(0, 0, working.clone())], VertexType::POSITION)?;
            match voxel::remesh(
                &surface,
                working_layout,
                settings.voxel_size.0 as f64,
                settings.voxel_max_cells,
            ) {
                Ok(output) => {
                    working = output.geometry;
                    cells = output.cells;
                }
                Err(error) if error.is::<voxel::Limit>() => return Err(error),
                Err(error) => {
                    return assemble(
                        snapshots,
                        metric,
                        None,
                        format!("voxel-unsupported: {error}"),
                    );
                }
            }
        }
        let prepared = Surface::new([(0, 0, working.clone())], VertexType::POSITION)?;
        if settings.voxelize
            && let Err(error) = prepared.closed_components()
        {
            return assemble(
                snapshots,
                metric,
                None,
                format!("voxel-unsupported-output: {error}"),
            );
        }
        let preparation = if sampled {
            rounded_error(canonical.deviation(
                &prepared,
                if weighted {
                    // Native absolute errors scale with mesh extent. Convert the relative
                    // normal weight to the same mesh-local units before adding distances.
                    settings.normal_weight.0 as f64 * scale as f64
                } else {
                    0.0
                },
            ))?
        } else {
            0.0
        };
        let preparation_facts = Some(LodPreparation {
            error: preparation,
            voxel_cells: cells,
            working_triangles: working.indices().triangle_count(),
        });
        if preparation as f64 > budget {
            return assemble(
                snapshots,
                metric,
                preparation_facts,
                "preparation-error-budget-exhausted".into(),
            );
        }
        let (min, max) = prepared.bounds();
        let working_scale = (max - min).max_element() as f32;
        let remaining = (budget - preparation as f64).max(0.0);
        let ratio = if working_scale > 0.0 {
            remaining / working_scale as f64
        } else {
            0.0
        };
        let mut ratio32 = ratio as f32;
        if ratio32 as f64 > ratio {
            ratio32 = ratio32.next_down();
        }
        let mut simplifier = settings.clone();
        simplifier.target_error = OrderedFloat(ratio32);
        let (candidates, reason) = clustered_lods(working, &locks, &simplifier)?;
        let mut stop = reason.to_owned();
        let floor = settings.min_triangles.min(snapshots[0].triangle_count());
        for (ordinal, candidate) in candidates.iter().enumerate() {
            if ordinal == 0 && !settings.voxelize {
                continue;
            }
            let previous = snapshots.last().unwrap().triangle_count();
            if candidate.geometry().indices().triangle_count() < floor {
                stop = "candidate-below-canonical-triangle-floor".into();
                continue;
            }
            if candidate.geometry().indices().triangle_count() >= previous {
                continue;
            }
            let mut accumulated = preparation as f64 + candidate.error() as f64;
            if preparation > 0.0 && candidate.error() > 0.0 {
                accumulated = accumulated.next_up();
            }
            let error = rounded_error(accumulated)?;
            if error as f64 > budget {
                continue;
            }
            let mut diagnostics = SnapshotDiagnostics::default();
            match redistribute(
                candidate.geometry(),
                self.layout,
                sources,
                &reference,
                reference_offset + budget + f64::from(scale).max(f64::MIN_POSITIVE) * 1e-6,
                &mut diagnostics,
            )
            .and_then(|parts| {
                finalize_snapshot(parts, sources, self.layout, error, diagnostics, finalize)
            }) {
                Ok(snapshot) => {
                    ensure!(
                        snapshot.triangle_count()
                            == candidate.geometry().indices().triangle_count(),
                        "redistribution changed triangle count"
                    );
                    snapshots.push(snapshot);
                }
                Err(error) => {
                    stop = format!("candidate-rejected: {error}");
                }
            }
        }
        assemble(snapshots, metric, preparation_facts, stop)
    }
}

fn join(sources: &[Geometry], layout: VertexType, weld: bool) -> anyhow::Result<Geometry> {
    let mut bytes = Vec::new();
    let mut indices = Vec::new();
    for source in sources {
        let offset = u32::try_from(bytes.len() / layout.stride())?;
        for idx in source.indices().as_u32() {
            indices.push(
                idx.checked_add(offset)
                    .context("joined vertex count overflow")?,
            );
        }
        bytes.extend_from_slice(source.vertex_data());
    }
    let geometry = Geometry::new(&bytes, layout, IndexBuffer::new(&indices)?)?;
    if weld {
        let mut edges = BTreeMap::<[super::Position; 2], Vec<[u32; 2]>>::new();
        for face in indices.chunks_exact(3) {
            for edge in [[face[0], face[1]], [face[1], face[2]], [face[2], face[0]]] {
                let mut key = edge.map(|v| position_key(geometry.position(v)));
                key.sort();
                edges.entry(key).or_default().push(edge);
            }
        }
        let stride = layout.stride();
        let mut vertices = Union::new(geometry.vertex_count());
        for pair in edges.values().filter(|p| p.len() == 2) {
            let [a, b] = pair[0].map(|i| i as usize);
            let [d, c] = pair[1].map(|i| i as usize);
            if position_key(geometry.position(a as u32))
                != position_key(geometry.position(c as u32))
            {
                continue;
            }
            for (a, b) in [(a, c), (b, d)] {
                if bytes[a * stride..(a + 1) * stride] == bytes[b * stride..(b + 1) * stride] {
                    vertices.join(a, b);
                }
            }
        }
        for idx in &mut indices {
            *idx = vertices.root(*idx as usize) as u32;
        }
    }
    let (bytes, _) =
        super::super::MeshAsset::compact_mesh_indices(&mut indices, &bytes, layout.stride())?;
    Geometry::new(&bytes, layout, IndexBuffer::new(&indices)?)
}

fn redistribute(
    working: &Geometry,
    layout: VertexType,
    sources: &[LodSource<'_>],
    reference: &Surface,
    max_distance: f64,
    diagnostics: &mut SnapshotDiagnostics,
) -> anyhow::Result<Vec<Vec<u8>>> {
    let mut parts = vec![Vec::new(); sources.len()];
    for face in working.indices().as_u32().chunks_exact(3) {
        let mut corners = [face[0], face[1], face[2]].map(|idx| Vertex::read(working, idx));
        let points = corners.map(|v| v.position);
        let normal = (points[1] - points[0])
            .cross(points[2] - points[0])
            .try_normalize()
            .context("degenerate simplified triangle")?;
        let mut votes = BTreeMap::<(u8, usize), (f64, f64)>::new();
        for (bary, weight) in SAMPLES {
            let hit = reference
                .nearest(
                    interpolate(points, DVec3::from_array(bary)),
                    Some(normal),
                    None,
                )
                .context("no orientation-compatible correspondence")?;
            ensure!(
                hit.distance <= max_distance,
                "material correspondence exceeds distance budget"
            );
            diagnostics.max_projection_distance = diagnostics
                .max_projection_distance
                .max(rounded_error(hit.distance)?);
            diagnostics.ambiguous_samples += usize::from(hit.ambiguous);
            let source = &reference.faces[hit.face];
            let vote = votes.entry((source.material, source.member)).or_default();
            vote.0 += weight;
            vote.1 += weight * hit.distance;
        }
        let mut materials = BTreeMap::<u8, (f64, f64)>::new();
        for (&(material, _), &(weight, distance)) in &votes {
            let v = materials.entry(material).or_default();
            v.0 += weight;
            v.1 += distance;
        }
        diagnostics.mixed_material_triangles += usize::from(materials.len() > 1);
        let &material = materials
            .iter()
            .min_by(|(a, x), (b, y)| y.0.total_cmp(&x.0).then(x.1.total_cmp(&y.1)).then(a.cmp(b)))
            .unwrap()
            .0;
        let &(_, member) = votes
            .iter()
            .filter(|((m, _), _)| *m == material)
            .min_by(|(a, x), (b, y)| y.0.total_cmp(&x.0).then(x.1.total_cmp(&y.1)).then(a.cmp(b)))
            .unwrap()
            .0;
        if layout.contains(VertexType::TEXTURE0)
            || layout.contains(VertexType::TEXTURE1)
            || layout.contains(VertexType::TANGENT)
        {
            let center = reference
                .nearest(
                    (points[0] + points[1] + points[2]) / 3.0,
                    Some(normal),
                    Some((member, usize::MAX)),
                )
                .context("missing material chart")?;
            let chart = reference.faces[center.face].chart;
            for corner in &mut corners {
                diagnostics.transferred_corners += 1;
                let unconstrained = reference
                    .nearest(corner.position, Some(normal), Some((member, usize::MAX)))
                    .context("missing corner correspondence")?;
                diagnostics.uv_chart_conflicts +=
                    usize::from(reference.faces[unconstrained.face].chart != chart);
                let hit = reference
                    .nearest(corner.position, Some(normal), Some((member, chart)))
                    .context("missing chart correspondence")?;
                let source = &reference.faces[hit.face];
                for channel in 0..2 {
                    corner.uv[channel] = source.vertices[0].uv[channel] * hit.bary.x
                        + source.vertices[1].uv[channel] * hit.bary.y
                        + source.vertices[2].uv[channel] * hit.bary.z;
                }
            }
        }
        let delta = [
            corners[1].uv[0] - corners[0].uv[0],
            corners[2].uv[0] - corners[0].uv[0],
        ];
        let determinant = delta[0].perp_dot(delta[1]);
        let tangent = if determinant.abs() > 1e-20 {
            ((points[1] - points[0]) * delta[1].y - (points[2] - points[0]) * delta[0].y)
                / determinant
        } else {
            DVec3::ZERO
        };
        let bitangent = if determinant.abs() > 1e-20 {
            ((points[2] - points[0]) * delta[0].x - (points[1] - points[0]) * delta[1].x)
                / determinant
        } else {
            DVec3::ZERO
        };
        for (idx, mut corner) in corners.into_iter().enumerate() {
            if corner.normal.length_squared() == 0.0 {
                corner.normal = normal;
            }
            let n = corner.normal.normalize();
            let t = (tangent - n * n.dot(tangent))
                .try_normalize()
                .unwrap_or_else(|| n.any_orthonormal_vector());
            let sign = if n.cross(t).dot(bitangent) < 0.0 {
                -1.0
            } else {
                1.0
            };
            let offset = parts[member].len();
            corner.write(
                layout,
                [t.x as f32, t.y as f32, t.z as f32, sign],
                &mut parts[member],
            )?;
            if layout.contains(VertexType::PACKED_NORMAL) {
                let source_offset = face[idx] as usize * working.vertex_type().stride();
                parts[member][offset + 12..offset + 16].copy_from_slice(
                    &working.vertex_data()[source_offset + 12..source_offset + 16],
                );
            }
        }
    }
    Ok(parts)
}

fn finalize_snapshot(
    parts: Vec<Vec<u8>>,
    sources: &[LodSource<'_>],
    layout: VertexType,
    error: f32,
    diagnostics: SnapshotDiagnostics,
    settings: &LodFinalizeSettings,
) -> anyhow::Result<GroupSnapshot> {
    let mut partitions = Vec::new();
    for (part, source) in parts.into_iter().zip(sources) {
        if part.is_empty() {
            continue;
        }
        let stride = layout.stride();
        let mut unique = BTreeMap::<Vec<u8>, u32>::new();
        let mut bytes = Vec::new();
        let mut indices = Vec::new();
        for vertex in part.chunks_exact(stride) {
            let index = if let Some(&idx) = unique.get(vertex) {
                idx
            } else {
                let idx = u32::try_from(bytes.len() / stride)?;
                unique.insert(vertex.to_vec(), idx);
                bytes.extend_from_slice(vertex);
                idx
            };
            indices.push(index);
        }
        ensure!(
            settings
                .max_index
                .is_none_or(|max| bytes.len() / stride - 1 <= max as usize),
            "finalized partition exceeds max-index"
        );
        Geometry::validate_pair(&bytes, stride, &indices)?;
        if settings.optimize_vertex_cache {
            optimize_vertex_cache_in_place(&mut indices, bytes.len() / stride);
        }
        if settings.optimize {
            let vertices = VertexDataAdapter::new(&bytes, stride, 0)?;
            optimize_overdraw_in_place(&mut indices, &vertices, settings.overdraw_threshold);
            let (reordered, _) =
                super::super::MeshAsset::compact_mesh_indices(&mut indices, &bytes, stride)?;
            bytes = reordered;
        }
        partitions.push(LodPartition::new(
            source.primitive,
            Lod::new(
                Geometry::new(&bytes, layout, IndexBuffer::new(&indices)?)?,
                error,
            )?,
        ));
    }
    GroupSnapshot::new(partitions.into_boxed_slice(), diagnostics)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        buf::{LodRequest, LodSettings},
        mesh::{
            Mesh, Primitive,
            test::{grid, triangles},
        },
    };

    fn settings(text: &str) -> ResolvedLodSettings {
        toml::from_str::<LodSettings>(text)
            .unwrap()
            .resolve(&LodSettings::default())
            .unwrap()
    }

    fn split_grid() -> [Geometry; 2] {
        let source = grid(9, false);
        std::array::from_fn(|member| {
            let indices = source
                .indices()
                .as_u32()
                .chunks_exact(3)
                .filter(|f| {
                    let center = f.iter().map(|&i| source.position(i)).sum::<DVec3>() / 3.;
                    let island = (4.0..5.0).contains(&center.x) && (4.0..5.0).contains(&center.y);
                    island == (member == 1)
                })
                .flatten()
                .copied()
                .collect::<Vec<_>>();
            let mut bytes = source.vertex_data().to_vec();
            if member == 1 {
                for vertex in bytes.chunks_exact_mut(32) {
                    let value = f32::from_ne_bytes(vertex[24..28].try_into().unwrap()) + 10.;
                    vertex[24..28].copy_from_slice(&value.to_ne_bytes());
                }
            }
            Geometry::new(
                &bytes,
                source.vertex_type(),
                IndexBuffer::new(&indices).unwrap(),
            )
            .unwrap()
        })
    }

    fn sources(geometry: &[Geometry]) -> Vec<LodSource<'_>> {
        geometry
            .iter()
            .enumerate()
            .map(|(idx, g)| LodSource {
                primitive: PrimitiveId(idx as u32),
                material: idx as u8,
                geometry: g,
                reference: None,
            })
            .collect()
    }

    #[test]
    fn source_only_tangents_do_not_require_texture0() {
        let layout = VertexType::NORMAL | VertexType::TANGENT;
        let mut bytes = Vec::new();
        for position in [DVec3::ZERO, DVec3::X, DVec3::Y] {
            Vertex {
                position,
                normal: DVec3::Z,
                ..Vertex::default()
            }
            .write(layout, [1., 0., 0., 1.], &mut bytes)
            .unwrap();
        }
        let geometry =
            [Geometry::new(&bytes, layout, IndexBuffer::new(&[0, 1, 2]).unwrap()).unwrap()];
        let request = LodGroupRequest {
            layout,
            simplify: false,
        };
        let settings = settings("");
        let group = request
            .process(
                &sources(&geometry),
                &settings,
                &[],
                &LodFinalizeSettings::default(),
            )
            .unwrap();
        assert_eq!(group.metric(), LodMetric::SourceOnly);
        assert_eq!(group.snapshots().len(), 1);
        let source = group.snapshots()[0].partition(PrimitiveId(0)).unwrap();
        assert_eq!(source.geometry().vertex_data(), bytes);
        assert!(source.geometry().texture0().is_none());
        assert!(
            LodGroupRequest {
                simplify: true,
                ..request
            }
            .process(
                &sources(&geometry),
                &settings,
                &[],
                &LodFinalizeSettings::default(),
            )
            .unwrap_err()
            .to_string()
            .contains("tangent reconstruction requires source texture0")
        );
    }

    #[test]
    fn unrequested_uv_channels_do_not_split_transfer_charts() {
        let grid = grid(9, false);
        let source_layout = VertexType::NORMAL | VertexType::TEXTURE0 | VertexType::TEXTURE1;
        for (channel, flag) in [VertexType::TEXTURE0, VertexType::TEXTURE1]
            .into_iter()
            .enumerate()
        {
            let layout = VertexType::NORMAL | flag;
            let mut bytes = Vec::new();
            for (chart, face) in grid.indices().as_u32().chunks_exact(3).enumerate() {
                for (corner, &index) in face.iter().enumerate() {
                    let mut vertex = Vertex::read(&grid, index);
                    vertex.uv[channel] = vertex.position.truncate() / 8.;
                    vertex.uv[1 - channel] = glam::DVec2::new(
                        chart as f64 * 2. + f64::from(corner == 1),
                        f64::from(corner == 2),
                    );
                    vertex.write(source_layout, [0.; 4], &mut bytes).unwrap();
                }
            }
            let indices = (0..(bytes.len() / source_layout.stride()) as u32).collect::<Vec<_>>();
            let full =
                Geometry::new(&bytes, source_layout, IndexBuffer::new(&indices).unwrap()).unwrap();
            let settings =
                settings("join=true\nmin-triangles=2\ntarget-error=0.1\nnormal-weight=0.0");
            for geometry in [full.project(layout).unwrap(), full] {
                let group = LodGroupRequest {
                    layout,
                    simplify: true,
                }
                .process(
                    &sources(&[geometry]),
                    &settings,
                    &[],
                    &LodFinalizeSettings::default(),
                )
                .unwrap();
                let last = group.snapshots().last().unwrap();
                assert_eq!(last.triangle_count(), 2);
                for snapshot in group.snapshots().iter().skip(1) {
                    assert_eq!(snapshot.diagnostics().uv_chart_conflicts, 0);
                    let geometry = snapshot.partition(PrimitiveId(0)).unwrap().geometry();
                    for index in 0..geometry.vertex_count() as u32 {
                        let vertex = Vertex::read(geometry, index);
                        assert!(
                            vertex.uv[channel].abs_diff_eq(vertex.position.truncate() / 8., 1e-6)
                        );
                    }
                }
            }
        }
    }

    #[test]
    fn skinny_faces_keep_their_material_after_reduction() {
        let locked = [[0., 0., 0.], [1_000_000., 0., 0.], [1_000_000., 1., 0.]];
        let points = locked.map(|p| DVec3::from_array(p.map(f64::from)));
        let mut bytes = Vec::new();
        for position in points {
            Vertex {
                position,
                ..Vertex::default()
            }
            .write(VertexType::POSITION, [0.; 4], &mut bytes)
            .unwrap();
        }
        let first = Geometry::new(
            &bytes,
            VertexType::POSITION,
            IndexBuffer::new(&[0, 1, 2]).unwrap(),
        )
        .unwrap();
        let mut bytes = Vec::new();
        let mut indices = Vec::new();
        for (bary, _) in SAMPLES {
            let center = interpolate(points, DVec3::from_array(bary))
                .as_vec3()
                .as_dvec3();
            let offset = (bytes.len() / 12) as u32;
            for [x, y] in [[-0.25, -0.25], [0.25, -0.25], [-0.25, 0.25], [0.25, 0.25]] {
                Vertex {
                    position: center + DVec3::new(x, y, 0.1),
                    ..Vertex::default()
                }
                .write(VertexType::POSITION, [0.; 4], &mut bytes)
                .unwrap();
            }
            indices.extend([
                offset,
                offset + 1,
                offset + 2,
                offset + 1,
                offset + 3,
                offset + 2,
            ]);
        }
        let second = Geometry::new(
            &bytes,
            VertexType::POSITION,
            IndexBuffer::new(&indices).unwrap(),
        )
        .unwrap();
        let group = LodGroupRequest {
            layout: VertexType::POSITION,
            simplify: true,
        }
        .process(
            &sources(&[first, second]),
            &settings("join=true\nmin-triangles=1\ntarget-error=0.1"),
            &locked,
            &LodFinalizeSettings::default(),
        )
        .unwrap();
        assert!(group.snapshots().len() > 1);
        for snapshot in group.snapshots().iter().skip(1) {
            let first = snapshot.partition(PrimitiveId(0)).unwrap().geometry();
            assert_eq!(first.indices().triangle_count(), 1);
            assert_eq!(
                first
                    .indices()
                    .as_u32()
                    .into_iter()
                    .map(|index| position_key(first.position(index)))
                    .collect::<BTreeSet<_>>(),
                points.map(position_key).into_iter().collect()
            );
            for partition in snapshot.partitions().iter().skip(1) {
                let geometry = partition.lod().geometry();
                assert!(
                    geometry
                        .indices()
                        .as_u32()
                        .into_iter()
                        .all(|index| geometry.position(index).z > 0.)
                );
            }
        }
    }

    #[test]
    fn join_redistributes_complete_snapshots_and_removes_material_only_locks() {
        let geometry = split_grid();
        let sources = sources(&geometry);
        let settings = settings("join=true\nmin-triangles=2\ntarget-error=0.1\nlock-border=true");
        for layout in [
            VertexType::POSITION,
            VertexType::PACKED_NORMAL,
            VertexType::NORMAL | VertexType::TEXTURE0,
        ] {
            let request = LodGroupRequest {
                layout,
                simplify: true,
            };
            let result = request
                .process(&sources, &settings, &[], &LodFinalizeSettings::default())
                .unwrap();
            assert!(
                result.snapshots().len() > 1,
                "{}",
                result.provenance().stop_reason
            );
            assert_eq!(
                result,
                request
                    .process(&sources, &settings, &[], &LodFinalizeSettings::default())
                    .unwrap()
            );
            let mut mesh = Mesh::new(
                geometry
                    .iter()
                    .enumerate()
                    .map(|(idx, g)| Primitive::new(idx as u8, g.clone()).unwrap())
                    .collect(),
                None,
            )
            .unwrap();
            mesh.set_lod_groups(vec![result.clone()].into_boxed_slice())
                .unwrap();
            for (idx, g) in geometry.iter().enumerate() {
                let first = result.snapshots()[0]
                    .partition(PrimitiveId(idx as u32))
                    .unwrap();
                assert_eq!(
                    triangles(first.geometry(), layout.contains(VertexType::TEXTURE0)),
                    triangles(
                        &g.project(layout).unwrap(),
                        layout.contains(VertexType::TEXTURE0)
                    )
                );
            }
            for snapshot in result.snapshots().iter().skip(1) {
                let mut total = 0;
                for partition in snapshot.partitions() {
                    let g = partition.lod().geometry();
                    total += g.indices().triangle_count();
                    assert_eq!(
                        g.indices().as_u32().into_iter().max().unwrap() as usize + 1,
                        g.vertex_count()
                    );
                    assert_eq!(
                        partition
                            .lod()
                            .patches()
                            .iter()
                            .map(|p| p.indices().len())
                            .sum::<usize>(),
                        g.indices().index_count()
                    );
                    if let Some(uv) = g.texture0() {
                        for value in uv {
                            assert!((value[0] >= 10.) == (partition.primitive() == PrimitiveId(1)));
                        }
                    }
                }
                assert_eq!(total, snapshot.triangle_count());
            }
            if layout == VertexType::POSITION {
                let mut separate_settings = settings.clone();
                separate_settings.join = false;
                let separate = geometry
                    .iter()
                    .map(|g| {
                        LodRequest {
                            layout,
                            simplify: true,
                        }
                        .process(g.project(layout).unwrap(), &separate_settings, &[])
                        .unwrap()
                        .levels()
                        .last()
                        .unwrap()
                        .geometry()
                        .indices()
                        .triangle_count()
                    })
                    .sum::<usize>();
                assert!(result.snapshots().last().unwrap().triangle_count() < separate);
                assert!(
                    result
                        .snapshots()
                        .last()
                        .unwrap()
                        .partition(PrimitiveId(1))
                        .is_none()
                );
            }
        }
    }

    pub(super) fn cube(side: u32) -> Geometry {
        let mut bytes = Vec::new();
        let mut indices = Vec::new();
        for (origin, u, v) in [
            ([-1., -1., 1.], [2., 0., 0.], [0., 2., 0.]),
            ([-1., 1., -1.], [2., 0., 0.], [0., -2., 0.]),
            ([1., -1., -1.], [0., 2., 0.], [0., 0., 2.]),
            ([-1., -1., 1.], [0., 2., 0.], [0., 0., -2.]),
            ([-1., 1., 1.], [2., 0., 0.], [0., 0., -2.]),
            ([-1., -1., -1.], [2., 0., 0.], [0., 0., 2.]),
        ] {
            let origin = DVec3::from_array(origin);
            let u = DVec3::from_array(u);
            let v = DVec3::from_array(v);
            let offset = (bytes.len() / 24) as u32;
            for y in 0..side {
                for x in 0..side {
                    Vertex {
                        position: origin
                            + u * (x as f64 / (side - 1) as f64)
                            + v * (y as f64 / (side - 1) as f64),
                        normal: u.cross(v).normalize(),
                        ..Vertex::default()
                    }
                    .write(VertexType::NORMAL, [0.; 4], &mut bytes)
                    .unwrap();
                }
            }
            for y in 0..side - 1 {
                for x in 0..side - 1 {
                    let i = offset + y * side + x;
                    indices.extend([i, i + 1, i + side, i + 1, i + side + 1, i + side]);
                }
            }
        }
        Geometry::new(
            &bytes,
            VertexType::NORMAL,
            IndexBuffer::new(&indices).unwrap(),
        )
        .unwrap()
    }

    #[test]
    fn voxelization_is_bounded_smooth_and_charges_preparation_error() {
        let geometry = [cube(5)];
        let sources = sources(&geometry);
        let settings = settings(
            "voxelize=true\nvoxel-size=0.4\nmin-triangles=8\ntarget-error=0.5\nnormal-weight=0.0",
        );
        let request = LodGroupRequest {
            layout: VertexType::POSITION,
            simplify: true,
        };
        let result = request
            .process(&sources, &settings, &[], &LodFinalizeSettings::default())
            .unwrap();
        assert_eq!(result.metric(), LodMetric::SampledGeometricAbsolute);
        assert!(
            result.snapshots().len() > 1,
            "{}",
            result.provenance().stop_reason
        );
        assert_eq!(
            result,
            request
                .process(&sources, &settings, &[], &LodFinalizeSettings::default())
                .unwrap()
        );
        for snapshot in result.snapshots().iter().skip(1) {
            assert!(result.preparation().unwrap().voxel_cells > 0);
            assert!(result.preparation().unwrap().error > 0.);
            assert!(snapshot.error() >= result.preparation().unwrap().error);
            assert!(snapshot.error() <= 1.);
            assert!(snapshot.triangle_count() < 192);
        }
        let mut limited = settings.clone();
        limited.voxel_max_cells = 2;
        assert!(
            request
                .process(&sources, &limited, &[], &LodFinalizeSettings::default())
                .unwrap_err()
                .is::<voxel::Limit>()
        );
        let locked = request
            .process(
                &sources,
                &settings,
                &[[1., 1., 1.]],
                &LodFinalizeSettings::default(),
            )
            .unwrap();
        assert_eq!(locked.snapshots().len(), 1);
        assert_eq!(locked.provenance().stop_reason, "voxel-external-locks");
        let open = [grid(5, false)];
        let open = request
            .process(
                &self::sources(&open),
                &settings,
                &[],
                &LodFinalizeSettings::default(),
            )
            .unwrap();
        assert_eq!(open.snapshots().len(), 1);
        assert!(open.provenance().stop_reason.contains("closed"));
    }

    #[test]
    fn weighted_preparation_error_scales_with_geometry_and_voxel_size() {
        let request = LodGroupRequest {
            layout: VertexType::PACKED_NORMAL,
            simplify: true,
        };
        let results = [1.0f32, 0.01].map(|scale| {
            let cube = cube(5);
            let mut bytes = cube.vertex_data().to_vec();
            for vertex in bytes.chunks_exact_mut(cube.vertex_type().stride()) {
                for word in vertex[..12].chunks_exact_mut(4) {
                    let value = f32::from_ne_bytes(word.try_into().unwrap()) * scale;
                    word.copy_from_slice(&value.to_ne_bytes());
                }
            }
            let geometry = [Geometry::new(&bytes, cube.vertex_type(), cube.indices().clone()).unwrap()];
            request.process(
                &sources(&geometry),
                &settings(&format!(
                    "join=true\nvoxelize=true\nvoxel-size={}\nmin-triangles=8\ntarget-error=1.0\nnormal-weight=0.5",
                    0.4 * scale
                )),
                &[],
                &LodFinalizeSettings::default(),
            ).unwrap()
        });
        for result in &results {
            assert!(
                result.snapshots().len() > 1,
                "{}",
                result.provenance().stop_reason
            );
        }
        let large = results[0].preparation().unwrap();
        let small = results[1].preparation().unwrap();
        assert!((large.error - small.error / 0.01).abs() < 1e-5);
        assert_eq!(large.voxel_cells, small.voxel_cells);
        assert_eq!(large.working_triangles, small.working_triangles);
    }

    #[test]
    fn rejected_preparation_retains_measurements_without_changing_canonical_error() {
        let geometry = [cube(5)];
        let result = LodGroupRequest {
            layout: VertexType::PACKED_NORMAL,
            simplify: true,
        }.process(
            &sources(&geometry),
            &settings("voxelize=true\nvoxel-size=0.4\nmin-triangles=8\ntarget-error=0.0001\nnormal-weight=0.5"),
            &[],
            &LodFinalizeSettings::default(),
        ).unwrap();
        assert_eq!(
            result.provenance().stop_reason,
            "preparation-error-budget-exhausted"
        );
        assert_eq!(result.snapshots().len(), 1);
        assert_eq!(result.snapshots()[0].error(), 0.0);
        let facts = result.preparation().unwrap();
        assert!(facts.error > 0.0002);
        assert!(facts.voxel_cells > 0 && facts.working_triangles > 0);
        let bytes = bincode::serde::encode_to_vec(&result, bincode::config::legacy()).unwrap();
        let (decoded, _): (LodGroup, _) =
            bincode::serde::decode_from_slice(&bytes, bincode::config::legacy()).unwrap();
        assert_eq!(decoded, result);
        let unsupported = LodGroupRequest {
            layout: VertexType::POSITION,
            simplify: true,
        }
        .process(
            &sources(&geometry),
            &settings("voxelize=true\nvoxel-size=0.4"),
            &[[1., 1., 1.]],
            &LodFinalizeSettings::default(),
        )
        .unwrap();
        assert!(unsupported.preparation().is_none());
    }

    #[test]
    fn orientation_matching_avoids_the_nearer_back_of_a_thin_wall() {
        let front = grid(2, false).project(VertexType::POSITION).unwrap();
        let mut bytes = front.vertex_data().to_vec();
        for v in bytes.chunks_exact_mut(12) {
            v[8..12].copy_from_slice(&(-0.02f32).to_ne_bytes());
        }
        let mut reverse = front.indices().as_u32();
        for f in reverse.chunks_exact_mut(3) {
            f.swap(1, 2);
        }
        let back = Geometry::new(
            &bytes,
            VertexType::POSITION,
            IndexBuffer::new(&reverse).unwrap(),
        )
        .unwrap();
        for v in bytes.chunks_exact_mut(12) {
            v[8..12].copy_from_slice(&(-0.019f32).to_ne_bytes());
        }
        let selected =
            Geometry::new(&bytes, VertexType::POSITION, front.indices().clone()).unwrap();
        let geometry = [front, back];
        let surface = Surface::new(
            geometry
                .iter()
                .enumerate()
                .map(|(idx, g)| (idx, idx as u8, g.clone())),
            VertexType::POSITION,
        )
        .unwrap();
        let mut diagnostics = SnapshotDiagnostics::default();
        let parts = redistribute(
            &selected,
            VertexType::POSITION,
            &sources(&geometry),
            &surface,
            0.1,
            &mut diagnostics,
        )
        .unwrap();
        assert!(!parts[0].is_empty() && parts[1].is_empty());
        assert!(diagnostics.max_projection_distance > 0.018);
    }

    #[test]
    fn overlapping_voxel_shells_have_no_interior_boundary_sheets() {
        let first = cube(3);
        let mut bytes = first.vertex_data().to_vec();
        for vertex in bytes.chunks_exact_mut(24) {
            let x = f32::from_ne_bytes(vertex[..4].try_into().unwrap()) + 0.25;
            vertex[..4].copy_from_slice(&x.to_ne_bytes());
        }
        let second = Geometry::new(&bytes, VertexType::NORMAL, first.indices().clone()).unwrap();
        let source = Surface::new([(0, 0, first), (1, 1, second)], VertexType::POSITION).unwrap();
        let components = source.closed_components().unwrap();
        assert!(
            source
                .signed_distance(DVec3::new(-0.75, 0., 0.), &components, 1e-12)
                .unwrap()
                < 0.
        );
        assert_eq!(
            source
                .signed_distance(DVec3::new(-1., 0., 0.), &components, 1e-12)
                .unwrap(),
            0.
        );
        let remeshed = voxel::remesh(&source, VertexType::PACKED_NORMAL, 0.5, 10_000).unwrap();
        let result = Surface::new([(0, 0, remeshed.geometry)], VertexType::POSITION).unwrap();
        result.closed_components().unwrap();
        for face in &result.faces {
            let center = face.points().into_iter().sum::<DVec3>() / 3.;
            assert!(
                center.x <= -0.95
                    || center.x >= 1.2
                    || center.y.abs() >= 0.7
                    || center.z.abs() >= 0.7,
                "interior triangle at {center:?}"
            );
        }
    }

    #[test]
    fn attribute_transfer_stays_in_one_chart_and_rebuilds_tangent_frames() {
        let grid = grid(9, false);
        let layout =
            VertexType::NORMAL | VertexType::TEXTURE0 | VertexType::TEXTURE1 | VertexType::TANGENT;
        let mut bytes = Vec::new();
        for face in grid.indices().as_u32().chunks_exact(3) {
            let right = face.iter().map(|&idx| grid.position(idx).x).sum::<f64>() / 3. >= 4.;
            for &index in face {
                let mut v = Vertex::read(&grid, index);
                if right {
                    v.uv[0].x += 10.;
                }
                v.uv[1] = v.uv[0] * 2.;
                v.write(layout, [1., 0., 0., 1.], &mut bytes).unwrap();
            }
        }
        let indices = (0..(bytes.len() / layout.stride()) as u32).collect::<Vec<_>>();
        let geometry =
            [Geometry::new(&bytes, layout, IndexBuffer::new(&indices).unwrap()).unwrap()];
        let group = LodGroupRequest {
            layout,
            simplify: true,
        }
        .process(
            &sources(&geometry),
            &settings("join=true\nmin-triangles=2\nlock-border=true\ntarget-error=0.1"),
            &[],
            &LodFinalizeSettings::default(),
        )
        .unwrap();
        assert!(
            group.snapshots().len() > 1,
            "{}",
            group.provenance().stop_reason
        );
        let snapshot = group.snapshots().last().unwrap();
        assert!(snapshot.diagnostics().uv_chart_conflicts > 0);
        assert_eq!(
            snapshot.diagnostics().transferred_corners,
            snapshot.triangle_count() * 3
        );
        for part in snapshot.partitions() {
            let g = part.lod().geometry();
            for face in g.indices().as_u32().chunks_exact(3) {
                let corners = [face[0], face[1], face[2]].map(|i| Vertex::read(g, i));
                assert!(
                    corners.iter().all(|v| v.uv[0].x < 1.)
                        || corners.iter().all(|v| v.uv[0].x >= 10.)
                );
                for (idx, v) in face.iter().zip(corners) {
                    assert_eq!(v.uv[1], v.uv[0] * 2.);
                    let offset = *idx as usize * layout.stride() + 40;
                    let tangent: [f32; 4] = std::array::from_fn(|i| {
                        f32::from_ne_bytes(
                            g.vertex_data()[offset + i * 4..offset + i * 4 + 4]
                                .try_into()
                                .unwrap(),
                        )
                    });
                    let t = DVec3::new(tangent[0] as f64, tangent[1] as f64, tangent[2] as f64);
                    assert!(
                        t.is_finite()
                            && (t.length() - 1.).abs() < 1e-5
                            && v.normal.dot(t).abs() < 1e-5
                    );
                    assert_eq!(tangent[3].abs(), 1.);
                }
            }
        }
    }

    #[test]
    fn coincident_materials_have_deterministic_owners_and_report_ambiguity() {
        let triangle = grid(2, false).project(VertexType::POSITION).unwrap();
        let geometry = vec![triangle.clone(); 10];
        let surface = Surface::new(
            geometry
                .iter()
                .enumerate()
                .map(|(i, g)| (i, i as u8, g.clone())),
            VertexType::POSITION,
        )
        .unwrap();
        let mut diagnostics = SnapshotDiagnostics::default();
        let parts = redistribute(
            &triangle,
            VertexType::POSITION,
            &sources(&geometry),
            &surface,
            0.01,
            &mut diagnostics,
        )
        .unwrap();
        assert!(!parts[0].is_empty() && parts[1..].iter().all(Vec::is_empty));
        assert_eq!(
            diagnostics.ambiguous_samples,
            triangle.indices().triangle_count() * 7
        );
        assert!(surface.closed_components().is_err());
    }

    #[test]
    fn join_normal_projection_does_not_charge_adjacent_hard_face_normals() {
        let geometry = [cube(3)];
        let group = LodGroupRequest {
            layout: VertexType::NORMAL,
            simplify: true,
        }
        .process(
            &sources(&geometry),
            &settings("join=true\nmin-triangles=12\ntarget-error=0.05"),
            &[],
            &LodFinalizeSettings::default(),
        )
        .unwrap();
        assert!(
            group.snapshots().len() > 1,
            "{}",
            group.provenance().stop_reason
        );
        assert!(group.preparation().unwrap().error < 1e-5);
    }

    #[test]
    fn grouped_settings_validate_and_explicit_false_overrides_pack_defaults() {
        let defaults: LodSettings =
            toml::from_str("join=true\nvoxelize=true\nvoxel-size=0.25").unwrap();
        let overrides: LodSettings = toml::from_str("join=false\nvoxelize=false").unwrap();
        let resolved = overrides.resolve(&defaults).unwrap();
        assert!(!resolved.join && !resolved.voxelize && resolved.voxel_size.0 == 0.25);
        for invalid in [
            "voxelize=true",
            "voxel-size=nan",
            "voxel-size=-1",
            "voxel-max-cells=0",
        ] {
            assert!(
                toml::from_str::<LodSettings>(invalid)
                    .unwrap()
                    .resolve(&LodSettings::default())
                    .is_err()
            );
        }
        let geometry = split_grid();
        assert!(
            LodGroupRequest {
                layout: VertexType::POSITION,
                simplify: true
            }
            .process(
                &sources(&geometry),
                &resolved,
                &[],
                &LodFinalizeSettings::default()
            )
            .is_err()
        );
        let joined = settings("join=true");
        assert!(
            LodRequest {
                layout: VertexType::POSITION,
                simplify: true
            }
            .process(
                geometry[0].project(VertexType::POSITION).unwrap(),
                &joined,
                &[]
            )
            .is_err()
        );
    }
}
