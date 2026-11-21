use {
    super::{Position, position_key},
    crate::mesh::{Geometry, VertexType},
    anyhow::ensure,
    glam::{DVec2, DVec3},
    std::collections::{BTreeMap, BTreeSet},
};

// Seven-point degree-five triangle quadrature. All samples are interior, avoiding edge ownership ties.
pub(super) const SAMPLES: [([f64; 3], f64); 7] = [
    ([1.0 / 3.0; 3], 0.225),
    (
        [0.059715871789770, 0.470142064105115, 0.470142064105115],
        0.132394152788506,
    ),
    (
        [0.470142064105115, 0.059715871789770, 0.470142064105115],
        0.132394152788506,
    ),
    (
        [0.470142064105115, 0.470142064105115, 0.059715871789770],
        0.132394152788506,
    ),
    (
        [0.797426985353087, 0.101286507323456, 0.101286507323456],
        0.125939180544827,
    ),
    (
        [0.101286507323456, 0.797426985353087, 0.101286507323456],
        0.125939180544827,
    ),
    (
        [0.101286507323456, 0.101286507323456, 0.797426985353087],
        0.125939180544827,
    ),
];

#[derive(Clone, Copy, Default)]
pub(super) struct Vertex {
    pub position: DVec3,
    pub normal: DVec3,
    pub uv: [DVec2; 2],
}

impl Vertex {
    pub fn read(geometry: &Geometry, index: u32) -> Self {
        let layout = geometry.vertex_type();
        let bytes = &geometry.vertex_data()[index as usize * layout.stride()..];
        let scalar =
            |offset| f32::from_ne_bytes(bytes[offset..offset + 4].try_into().unwrap()) as f64;
        let mut result = Self {
            position: geometry.position(index),
            ..Self::default()
        };
        let mut offset = 12;
        if layout.contains(VertexType::NORMAL) {
            result.normal = DVec3::new(scalar(offset), scalar(offset + 4), scalar(offset + 8));
            offset += 12;
        } else if layout.contains(VertexType::PACKED_NORMAL) {
            let x =
                (i16::from_ne_bytes(bytes[12..14].try_into().unwrap()) as f64 / 32767.0).max(-1.0);
            let y =
                (i16::from_ne_bytes(bytes[14..16].try_into().unwrap()) as f64 / 32767.0).max(-1.0);
            let mut n = DVec3::new(x, y, 1.0 - x.abs() - y.abs());
            let t = (-n.z).max(0.0);
            n.x += if x >= 0.0 { -t } else { t };
            n.y += if y >= 0.0 { -t } else { t };
            result.normal = n.normalize_or_zero();
            offset += 4;
        }
        for (idx, flag) in [VertexType::TEXTURE0, VertexType::TEXTURE1]
            .into_iter()
            .enumerate()
        {
            if layout.contains(flag) {
                result.uv[idx] = DVec2::new(scalar(offset), scalar(offset + 4));
                offset += 8;
            }
        }
        result
    }

    pub fn write(
        &self,
        layout: VertexType,
        tangent: [f32; 4],
        bytes: &mut Vec<u8>,
    ) -> anyhow::Result<()> {
        let append = |bytes: &mut Vec<u8>, values: &[f64]| -> anyhow::Result<()> {
            for value in values {
                let value = *value as f32;
                ensure!(value.is_finite(), "nonfinite transferred vertex attribute");
                bytes.extend_from_slice(&value.to_ne_bytes());
            }
            Ok(())
        };
        append(bytes, &self.position.to_array())?;
        let normal = self.normal.normalize_or_zero();
        if layout.contains(VertexType::NORMAL) {
            append(bytes, &normal.to_array())?;
        }
        if layout.contains(VertexType::PACKED_NORMAL) {
            ensure!(
                normal.is_finite() && normal.length_squared() > 0.0,
                "invalid remeshed normal"
            );
            let n = normal / normal.abs().element_sum();
            let oct = if n.z < 0.0 {
                [
                    (1.0 - n.y.abs()) * n.x.signum(),
                    (1.0 - n.x.abs()) * n.y.signum(),
                ]
            } else {
                [n.x, n.y]
            };
            for v in oct {
                bytes.extend_from_slice(&((v * 32767.0).round() as i16).to_ne_bytes());
            }
        }
        for (idx, flag) in [VertexType::TEXTURE0, VertexType::TEXTURE1]
            .into_iter()
            .enumerate()
        {
            if layout.contains(flag) {
                append(bytes, &self.uv[idx].to_array())?;
            }
        }
        if layout.contains(VertexType::TANGENT) {
            append(bytes, &tangent.map(f64::from))?;
        }
        Ok(())
    }
}

pub(super) struct Face {
    pub vertices: [Vertex; 3],
    pub member: usize,
    pub material: u8,
    pub chart: usize,
    pub normal: DVec3,
}

impl Face {
    pub fn points(&self) -> [DVec3; 3] {
        self.vertices.map(|v| v.position)
    }
}

pub(super) struct Hit {
    pub face: usize,
    pub bary: DVec3,
    pub distance: f64,
    pub ambiguous: bool,
}

struct Node {
    min: DVec3,
    max: DVec3,
    range: std::ops::Range<usize>,
    children: Option<[usize; 2]>,
}

pub(super) struct Surface {
    pub faces: Vec<Face>,
    nodes: Vec<Node>,
    order: Vec<usize>,
}

impl Surface {
    pub fn new(
        sources: impl IntoIterator<Item = (usize, u8, Geometry)>,
        chart_layout: VertexType,
    ) -> anyhow::Result<Self> {
        let chart_channels = [
            chart_layout.contains(VertexType::TEXTURE0)
                || chart_layout.contains(VertexType::TANGENT),
            chart_layout.contains(VertexType::TEXTURE1),
        ];
        let mut faces = Vec::new();
        for (member, material, geometry) in sources {
            let start = faces.len();
            let indices = geometry.indices().as_u32();
            let mut charts = Union::new(indices.len() / 3);
            let mut edges = BTreeMap::<[[u32; 7]; 2], Vec<(usize, bool)>>::new();
            for (idx, face) in indices.chunks_exact(3).enumerate() {
                let vertices = [face[0], face[1], face[2]].map(|idx| Vertex::read(&geometry, idx));
                // Only transferred UVs and tangent reconstruction constrain chart membership.
                let keys = vertices.map(|v| {
                    let p = position_key(v.position);
                    let uv = std::array::from_fn::<_, 2, _>(|channel| {
                        if chart_channels[channel] {
                            v.uv[channel]
                        } else {
                            DVec2::ZERO
                        }
                    });
                    let uv = [uv[0].x, uv[0].y, uv[1].x, uv[1].y]
                        .map(|v| if v == 0.0 { 0 } else { (v as f32).to_bits() });
                    [p[0], p[1], p[2], uv[0], uv[1], uv[2], uv[3]]
                });
                for [a, b] in [[keys[0], keys[1]], [keys[1], keys[2]], [keys[2], keys[0]]] {
                    let (key, forward) = if a < b {
                        ([a, b], true)
                    } else {
                        ([b, a], false)
                    };
                    edges.entry(key).or_default().push((idx, forward));
                }
                let normal = (vertices[1].position - vertices[0].position)
                    .cross(vertices[2].position - vertices[0].position)
                    .normalize_or_zero();
                faces.push(Face {
                    vertices,
                    member,
                    material,
                    chart: 0,
                    normal,
                });
            }
            for pair in edges
                .values()
                .filter(|pair| pair.len() == 2 && pair[0].1 != pair[1].1)
            {
                charts.join(pair[0].0, pair[1].0);
            }
            for idx in 0..indices.len() / 3 {
                faces[start + idx].chart = start + charts.root(idx);
            }
        }
        ensure!(!faces.is_empty(), "empty correspondence surface");
        let mut result = Self {
            order: (0..faces.len()).collect(),
            faces,
            nodes: Vec::new(),
        };
        result.build(0..result.faces.len());
        Ok(result)
    }

    fn build(&mut self, range: std::ops::Range<usize>) -> usize {
        let mut min = DVec3::splat(f64::INFINITY);
        let mut max = DVec3::splat(f64::NEG_INFINITY);
        for &idx in &self.order[range.clone()] {
            for p in self.faces[idx].points() {
                min = min.min(p);
                max = max.max(p);
            }
        }
        let idx = self.nodes.len();
        self.nodes.push(Node {
            min,
            max,
            range: range.clone(),
            children: None,
        });
        if range.len() > 8 {
            let extent = max - min;
            let axis = if extent.x >= extent.y && extent.x >= extent.z {
                0
            } else if extent.y >= extent.z {
                1
            } else {
                2
            };
            self.order[range.clone()].sort_by(|&a, &b| {
                let center = |idx: usize| {
                    self.faces[idx]
                        .points()
                        .iter()
                        .map(|p| p[axis])
                        .sum::<f64>()
                };
                center(a).total_cmp(&center(b)).then(a.cmp(&b))
            });
            let mid = range.start + range.len() / 2;
            self.nodes[idx].children =
                Some([self.build(range.start..mid), self.build(mid..range.end)]);
        }
        idx
    }

    pub fn bounds(&self) -> (DVec3, DVec3) {
        (self.nodes[0].min, self.nodes[0].max)
    }

    pub fn nearest(
        &self,
        point: DVec3,
        normal: Option<DVec3>,
        member_chart: Option<(usize, usize)>,
    ) -> Option<Hit> {
        self.nearest_where(point, |idx| {
            let face = &self.faces[idx];
            normal.is_none_or(|n| face.normal.dot(n) > 0.0)
                && member_chart
                    .is_none_or(|(m, c)| face.member == m && (c == usize::MAX || face.chart == c))
        })
    }

    fn nearest_where(&self, point: DVec3, accept: impl Fn(usize) -> bool) -> Option<Hit> {
        self.nearest_with_normal(point, accept, None)
    }

    fn nearest_with_normal(
        &self,
        point: DVec3,
        accept: impl Fn(usize) -> bool,
        normal: Option<DVec3>,
    ) -> Option<Hit> {
        let mut best: Option<Hit> = None;
        let mut best_normal_distance = f64::INFINITY;
        let mut stack = vec![0];
        while let Some(idx) = stack.pop() {
            let node = &self.nodes[idx];
            if best.as_ref().is_some_and(|hit| {
                point.distance_squared(point.clamp(node.min, node.max))
                    > hit.distance * hit.distance
            }) {
                continue;
            }
            if let Some([a, b]) = node.children {
                let distance = |i: usize| {
                    point.distance_squared(point.clamp(self.nodes[i].min, self.nodes[i].max))
                };
                if distance(a) <= distance(b) {
                    stack.extend([b, a]);
                } else {
                    stack.extend([a, b]);
                }
            } else {
                for &idx in &self.order[node.range.clone()] {
                    let face = &self.faces[idx];
                    if !accept(idx) {
                        continue;
                    }
                    let bary = closest_bary(point, face.points());
                    let projected = interpolate(face.points(), bary);
                    let distance = point.distance(projected);
                    let normal_distance = normal.map_or(0.0, |n| {
                        n.distance(
                            interpolate(face.vertices.map(|v| v.normal), bary).normalize_or_zero(),
                        )
                    });
                    let ambiguous = best.as_ref().is_some_and(|old| {
                        distance == old.distance
                            && (old.ambiguous || self.faces[old.face].material != face.material)
                    });
                    if best.as_ref().is_none_or(|old| {
                        (distance, normal_distance, idx)
                            < (old.distance, best_normal_distance, old.face)
                    }) {
                        best_normal_distance = normal_distance;
                        best = Some(Hit {
                            face: idx,
                            bary,
                            distance,
                            ambiguous,
                        });
                    } else if ambiguous {
                        best.as_mut().unwrap().ambiguous = true;
                    }
                }
            }
        }
        best
    }

    /// `normal_weight` converts a unit-normal difference to mesh-local distance units.
    pub fn deviation(&self, other: &Self, normal_weight: f64) -> f64 {
        let directed = |a: &Self, b: &Self| {
            let mut error = 0.0f64;
            for face in &a.faces {
                let points = face.points();
                for bary in [DVec3::X, DVec3::Y, DVec3::Z]
                    .into_iter()
                    .chain(SAMPLES.iter().map(|(b, _)| DVec3::from_array(*b)))
                {
                    let normal =
                        interpolate(face.vertices.map(|v| v.normal), bary).normalize_or_zero();
                    // At hard-normal seams, equal-distance faces are different attribute
                    // samples. Prefer the matching variant before charging preparation error.
                    let hit = b
                        .nearest_with_normal(
                            interpolate(points, bary),
                            |_| true,
                            (normal_weight > 0.0).then_some(normal),
                        )
                        .unwrap();
                    let other = interpolate(b.faces[hit.face].vertices.map(|v| v.normal), hit.bary)
                        .normalize_or_zero();
                    error = error.max(hit.distance + normal_weight * normal.distance(other));
                }
            }
            error
        };
        directed(self, other).max(directed(other, self))
    }

    /// Closed geometric components, including matching edges split by material/normal attributes.
    pub fn closed_components(&self) -> anyhow::Result<Vec<usize>> {
        let mut edges = BTreeMap::<[Position; 2], Vec<(usize, bool)>>::new();
        let mut components = Union::new(self.faces.len());
        for (idx, face) in self.faces.iter().enumerate() {
            ensure!(
                face.normal.length_squared() > 0.0,
                "voxel source contains degenerate faces"
            );
            let p = face.points().map(position_key);
            for [a, b] in [[p[0], p[1]], [p[1], p[2]], [p[2], p[0]]] {
                let (key, forward) = if a < b {
                    ([a, b], true)
                } else {
                    ([b, a], false)
                };
                edges.entry(key).or_default().push((idx, forward));
            }
        }
        for pair in edges.values() {
            ensure!(
                pair.len() == 2 && pair[0].1 != pair[1].1,
                "voxel source must be closed and consistently oriented"
            );
            components.join(pair[0].0, pair[1].0);
        }
        let components = (0..self.faces.len())
            .map(|i| components.root(i))
            .collect::<Vec<_>>();
        let mut volumes = BTreeMap::<usize, f64>::new();
        for (face, &component) in self.faces.iter().zip(&components) {
            // Translate first to avoid cancellation far from the mesh-local origin.
            let origin = self.faces[component].vertices[0].position;
            let [a, b, c] = face.points().map(|p| p - origin);
            *volumes.entry(component).or_default() += a.dot(b.cross(c));
        }
        ensure!(
            volumes.values().all(|&volume| volume > 0.0),
            "voxel shells must have outward winding and positive volume"
        );
        Ok(components)
    }

    /// Union of component signed-distance fields (the minimum signed value).
    /// A boundary of one component strictly inside another is still interior.
    pub fn signed_distance(
        &self,
        point: DVec3,
        components: &[usize],
        tolerance: f64,
    ) -> anyhow::Result<f64> {
        for direction in [
            DVec3::new(1.0, 0.3713906763541037, 0.6947465906068658),
            DVec3::new(0.52999894000318, 1.0, 0.847998304005088),
            DVec3::new(0.618033988749895, 0.414213562373095, 1.0),
        ] {
            let mut counts = BTreeMap::<usize, i32>::new();
            let mut boundary = BTreeSet::new();
            let mut ambiguous = false;
            let mut stack = vec![0];
            while let Some(idx) = stack.pop() {
                let node = &self.nodes[idx];
                let a = (node.min - DVec3::splat(tolerance) - point) / direction;
                let b = (node.max + DVec3::splat(tolerance) - point) / direction;
                if a.min(b).max_element().max(0.0) > a.max(b).min_element() {
                    continue;
                }
                if let Some(children) = node.children {
                    stack.extend(children);
                    continue;
                }
                for &idx in &self.order[node.range.clone()] {
                    let [a, b, c] = self.faces[idx].points();
                    let ab = b - a;
                    let ac = c - a;
                    let h = direction.cross(ac);
                    let determinant = ab.dot(h);
                    if determinant.abs() <= 1e-14 * ab.length() * ac.length() {
                        continue;
                    }
                    let delta = point - a;
                    let u = delta.dot(h) / determinant;
                    let q = delta.cross(ab);
                    let v = direction.dot(q) / determinant;
                    let t = ac.dot(q) / determinant;
                    if u < -1e-12 || v < -1e-12 || u + v > 1.0 + 1e-12 {
                        continue;
                    }
                    if t.abs() * direction.length() <= tolerance {
                        boundary.insert(components[idx]);
                        continue;
                    }
                    if t < 0.0 {
                        continue;
                    }
                    if u <= 1e-12 || v <= 1e-12 || 1.0 - u - v <= 1e-12 {
                        ambiguous = true;
                        break;
                    }
                    *counts.entry(components[idx]).or_default() +=
                        if determinant > 0.0 { 1 } else { -1 };
                }
                if ambiguous {
                    break;
                }
            }
            if !ambiguous {
                let mut interior_distance = 0.0f64;
                for (&component, &count) in &counts {
                    if count != 0 && !boundary.contains(&component) {
                        let hit = self
                            .nearest_where(point, |idx| components[idx] == component)
                            .unwrap();
                        interior_distance = interior_distance.max(hit.distance.max(tolerance));
                    }
                }
                return Ok(if interior_distance > 0.0 {
                    -interior_distance
                } else if !boundary.is_empty() {
                    0.0
                } else {
                    self.nearest(point, None, None).unwrap().distance
                });
            }
        }
        anyhow::bail!("ambiguous voxel interior")
    }
}

pub(super) struct Union(Vec<usize>);
impl Union {
    pub fn new(len: usize) -> Self {
        Self((0..len).collect())
    }
    pub fn root(&mut self, mut i: usize) -> usize {
        while self.0[i] != i {
            self.0[i] = self.0[self.0[i]];
            i = self.0[i];
        }
        i
    }
    pub fn join(&mut self, a: usize, b: usize) {
        let a = self.root(a);
        let b = self.root(b);
        self.0[a.max(b)] = a.min(b);
    }
}

pub(super) fn interpolate(points: [DVec3; 3], bary: DVec3) -> DVec3 {
    points[0] * bary.x + points[1] * bary.y + points[2] * bary.z
}

pub(super) fn closest_bary(p: DVec3, [a, b, c]: [DVec3; 3]) -> DVec3 {
    let ab = b - a;
    let ac = c - a;
    let normal = ab.cross(ac);
    if normal.length_squared() == 0.0 {
        return [
            (a, b, DVec3::X, DVec3::Y),
            (b, c, DVec3::Y, DVec3::Z),
            (c, a, DVec3::Z, DVec3::X),
        ]
        .into_iter()
        .map(|(a, b, u, v)| {
            let e = b - a;
            let t = if e.length_squared() > 0.0 {
                ((p - a).dot(e) / e.length_squared()).clamp(0.0, 1.0)
            } else {
                0.0
            };
            (p.distance_squared(a + e * t), u.lerp(v, t))
        })
        .min_by(|a, b| a.0.total_cmp(&b.0))
        .unwrap()
        .1;
    }
    let bc = c - b;
    let ap = p - a;
    let d1 = ab.dot(ap);
    let d2 = ac.dot(ap);
    if d1 <= 0.0 && d2 <= 0.0 {
        return DVec3::X;
    }
    let bp = p - b;
    let d3 = ab.dot(bp);
    let bp_bc = bc.dot(bp);
    if d3 >= 0.0 && bp_bc <= 0.0 {
        return DVec3::Y;
    }
    // Signed areas avoid cancellation between products of nearly parallel edge dot products.
    let vc = normal.dot(ab.cross(ap));
    if vc <= 0.0 && d1 >= 0.0 && d3 <= 0.0 {
        let v = d1 / (d1 - d3);
        return DVec3::new(1.0 - v, v, 0.0);
    }
    let cp = p - c;
    let d6 = ac.dot(cp);
    let cp_bc = bc.dot(cp);
    if d6 >= 0.0 && cp_bc >= 0.0 {
        return DVec3::Z;
    }
    let vb = normal.dot(ap.cross(ac));
    if vb <= 0.0 && d2 >= 0.0 && d6 <= 0.0 {
        let w = d2 / (d2 - d6);
        return DVec3::new(1.0 - w, 0.0, w);
    }
    let va = normal.dot(bp.cross(cp));
    if va <= 0.0 && bp_bc >= 0.0 && cp_bc <= 0.0 {
        let w = bp_bc / bc.length_squared();
        return DVec3::new(0.0, 1.0 - w, w);
    }
    DVec3::new(va, vb, vc) / (va + vb + vc)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::index::IndexBuffer;

    #[test]
    fn charts_preserve_only_required_uv_seams_including_tangent_inputs() {
        let layout =
            VertexType::NORMAL | VertexType::TEXTURE0 | VertexType::TEXTURE1 | VertexType::TANGENT;
        for channel in 0..2 {
            let mut bytes = Vec::new();
            for (face, positions) in [
                [[0., 0., 0.], [1., 0., 0.], [0., 1., 0.]],
                [[1., 0., 0.], [1., 1., 0.], [0., 1., 0.]],
            ]
            .into_iter()
            .enumerate()
            {
                for position in positions.map(DVec3::from_array) {
                    let mut vertex = Vertex {
                        position,
                        normal: DVec3::Z,
                        uv: [position.truncate(); 2],
                    };
                    vertex.uv[channel].x += face as f64 * 10.;
                    vertex.write(layout, [1., 0., 0., 1.], &mut bytes).unwrap();
                }
            }
            let geometry = Geometry::new(
                &bytes,
                layout,
                IndexBuffer::new(&[0, 1, 2, 3, 4, 5]).unwrap(),
            )
            .unwrap();
            for (requested, joined) in [
                (VertexType::POSITION, true),
                (VertexType::TEXTURE0, channel == 1),
                (VertexType::TEXTURE1, channel == 0),
                (VertexType::TEXTURE0 | VertexType::TEXTURE1, false),
                (VertexType::NORMAL | VertexType::TANGENT, channel == 1),
            ] {
                let surface = Surface::new([(0, 0, geometry.clone())], requested).unwrap();
                assert_eq!(surface.faces[0].chart == surface.faces[1].chart, joined);
            }
        }
    }

    #[test]
    fn closest_bary_handles_high_aspect_ratio_faces() {
        for length in [1., 1_000., 1_000_000., 1_000_000_000.] {
            let points = [
                DVec3::ZERO,
                DVec3::new(length, 0., 0.),
                DVec3::new(length, 1., 0.),
            ];
            for order in [
                [0, 1, 2],
                [1, 2, 0],
                [2, 0, 1],
                [0, 2, 1],
                [2, 1, 0],
                [1, 0, 2],
            ] {
                let points = order.map(|index| points[index]);
                for (bary, _) in SAMPLES {
                    let bary = DVec3::from_array(bary);
                    let point = interpolate(points, bary);
                    for distance in [0., 0.25, -1.] {
                        let query = point + DVec3::Z * distance;
                        let actual = closest_bary(query, points);
                        assert!(actual.is_finite() && actual.min_element() >= 0.);
                        assert!(
                            actual.abs_diff_eq(bary, 1e-12),
                            "{length}: {actual:?} != {bary:?}"
                        );
                        assert!(interpolate(points, actual).abs_diff_eq(point, length * 1e-12));
                    }
                }
                let bary = closest_bary(DVec3::new(length * 2., 0.25, 0.5), points);
                assert!(
                    interpolate(points, bary)
                        .abs_diff_eq(DVec3::new(length, 0.25, 0.), length * 1e-12)
                );
            }
        }
    }

    #[test]
    fn closest_bary_handles_edges_vertices_and_degenerate_faces() {
        let points = [DVec3::ZERO, DVec3::X, DVec3::Y];
        for (point, expected) in [
            (DVec3::new(-1., -1., 1.), DVec3::X),
            (DVec3::new(2., 0., 1.), DVec3::Y),
            (DVec3::new(0., 2., 1.), DVec3::Z),
            (DVec3::new(0.5, -1., 1.), DVec3::new(0.5, 0.5, 0.)),
            (DVec3::new(-1., 0.5, 1.), DVec3::new(0.5, 0., 0.5)),
            (DVec3::new(1., 1., 1.), DVec3::new(0., 0.5, 0.5)),
        ] {
            assert_eq!(closest_bary(point, points), expected);
        }
        for points in [[DVec3::ZERO, DVec3::X, DVec3::X * 2.], [DVec3::ZERO; 3]] {
            let bary = closest_bary(DVec3::new(0.5, 1., 0.), points);
            assert!(bary.is_finite() && bary.min_element() >= 0.);
            let expected = if points[1] == DVec3::ZERO {
                DVec3::ZERO
            } else {
                DVec3::X * 0.5
            };
            assert_eq!(interpolate(points, bary), expected);
        }
    }
}
