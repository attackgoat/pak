use {
    super::surface::{Surface, Vertex, closest_bary, interpolate},
    crate::{
        index::IndexBuffer,
        mesh::{Geometry, VertexType},
    },
    anyhow::{Context as _, ensure},
    glam::DVec3,
    std::collections::{BTreeMap, BTreeSet},
};

type Cell = [i64; 3];

#[derive(Debug)]
pub(super) struct Limit;
impl std::fmt::Display for Limit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("voxel work or cell limit exceeded")
    }
}
impl std::error::Error for Limit {}

pub(super) struct Output {
    pub geometry: Geometry,
    pub cells: usize,
}

/// Sparse surface-band signed distance reconstruction with a conforming six-tetrahedra grid.
/// Components use oriented ray winding independently, so overlapping closed shells form a union.
pub(super) fn remesh(
    source: &Surface,
    layout: VertexType,
    size: f64,
    max_cells: usize,
) -> anyhow::Result<Output> {
    let components = source.closed_components()?;
    let (min, max) = source.bounds();
    if ((max - min) / size).max_element() >= (1u64 << 40) as f64 {
        return Err(Limit.into());
    }
    let origin = min - DVec3::splat(size * 1.5);
    ensure!(origin.is_finite(), "invalid voxel grid origin");
    let point = |cell: Cell| origin + DVec3::from_array(cell.map(|v| v as f64)) * size;
    let mut cells = BTreeSet::new();
    let mut work = 0usize;
    let max_work = max_cells
        .checked_mul(64)
        .context("voxel work limit overflow")?;
    for face in &source.faces {
        let triangle = face.points();
        let lo = triangle
            .into_iter()
            .fold(DVec3::splat(f64::INFINITY), DVec3::min);
        let hi = triangle
            .into_iter()
            .fold(DVec3::splat(f64::NEG_INFINITY), DVec3::max);
        let lo = ((lo - origin) / size)
            .floor()
            .to_array()
            .map(|v| v as i64 - 1);
        let hi = ((hi - origin) / size)
            .ceil()
            .to_array()
            .map(|v| v as i64 + 1);
        let mut count = 1usize;
        for axis in 0..3 {
            count = count
                .checked_mul(usize::try_from(hi[axis] - lo[axis] + 1)?)
                .ok_or(Limit)?;
        }
        work = work.checked_add(count).ok_or(Limit)?;
        if work > max_work {
            return Err(Limit.into());
        }
        for z in lo[2]..=hi[2] {
            for y in lo[1]..=hi[1] {
                for x in lo[0]..=hi[0] {
                    let cell = [x, y, z];
                    let center = point(cell) + DVec3::splat(size * 0.5);
                    let nearest = interpolate(triangle, closest_bary(center, triangle));
                    if center.distance_squared(nearest) <= size * size * 3.0 {
                        cells.insert(cell);
                        if cells.len() > max_cells {
                            return Err(Limit.into());
                        }
                    }
                }
            }
        }
    }

    let mut field = BTreeMap::<Cell, f64>::new();
    let mut edges = BTreeMap::<(Cell, Cell), u32>::new();
    let mut positions = Vec::<DVec3>::new();
    let mut indices = Vec::new();
    for &cell in &cells {
        let corners: [Cell; 8] = std::array::from_fn(|i| {
            [
                cell[0] + (i & 1) as i64,
                cell[1] + ((i >> 1) & 1) as i64,
                cell[2] + ((i >> 2) & 1) as i64,
            ]
        });
        let mut values = [0.0; 8];
        for (idx, &corner) in corners.iter().enumerate() {
            values[idx] = if let Some(value) = field.get(&corner) {
                *value
            } else {
                let p = point(corner);
                let value = source.signed_distance(p, &components, size * 1e-12)?;
                field.insert(corner, value);
                value
            };
        }
        for tetrahedron in [
            [0, 1, 3, 7],
            [0, 3, 2, 7],
            [0, 2, 6, 7],
            [0, 6, 4, 7],
            [0, 4, 5, 7],
            [0, 5, 1, 7],
        ] {
            let inside = tetrahedron
                .into_iter()
                .filter(|&i| values[i] < 0.0)
                .collect::<Vec<_>>();
            let outside = tetrahedron
                .into_iter()
                .filter(|&i| values[i] >= 0.0)
                .collect::<Vec<_>>();
            if inside.is_empty() || outside.is_empty() {
                continue;
            }
            let direction = outside.iter().map(|&i| point(corners[i])).sum::<DVec3>()
                / outside.len() as f64
                - inside.iter().map(|&i| point(corners[i])).sum::<DVec3>() / inside.len() as f64;
            let pairs = if inside.len() == 1 {
                vec![
                    (inside[0], outside[0]),
                    (inside[0], outside[1]),
                    (inside[0], outside[2]),
                ]
            } else if outside.len() == 1 {
                vec![
                    (inside[0], outside[0]),
                    (inside[1], outside[0]),
                    (inside[2], outside[0]),
                ]
            } else {
                vec![
                    (inside[0], outside[0]),
                    (inside[0], outside[1]),
                    (inside[1], outside[1]),
                    (inside[1], outside[0]),
                ]
            };
            let mut polygon = Vec::with_capacity(pairs.len());
            for (a, b) in pairs {
                let (a, b) = if corners[a] <= corners[b] {
                    (a, b)
                } else {
                    (b, a)
                };
                let t = values[a] / (values[a] - values[b]);
                let key = if t == 0.0 {
                    (corners[a], corners[a])
                } else if t == 1.0 {
                    (corners[b], corners[b])
                } else {
                    (corners[a], corners[b])
                };
                let idx = if let Some(&idx) = edges.get(&key) {
                    idx
                } else {
                    let idx = u32::try_from(positions.len())?;
                    let p = point(corners[a]).lerp(point(corners[b]), t);
                    // Validate and measure the actual f32 output, not higher-precision working points.
                    let p = DVec3::from_array(p.to_array().map(|v| (v as f32) as f64));
                    ensure!(p.is_finite(), "nonfinite voxel position");
                    positions.push(p);
                    edges.insert(key, idx);
                    idx
                };
                polygon.push(idx);
            }
            for i in 1..polygon.len() - 1 {
                let mut face = [polygon[0], polygon[i], polygon[i + 1]];
                let [a, b, c] = face.map(|i| positions[i as usize]);
                let normal = (b - a).cross(c - a);
                if normal.length_squared() == 0.0 {
                    continue;
                }
                if normal.dot(direction) < 0.0 {
                    face.swap(1, 2);
                }
                indices.extend(face);
            }
        }
    }
    ensure!(!indices.is_empty(), "voxelization removed every surface");
    let mut normals = vec![DVec3::ZERO; positions.len()];
    for face in indices.chunks_exact(3) {
        let [a, b, c] = [face[0], face[1], face[2]].map(|i| positions[i as usize]);
        let normal = (b - a).cross(c - a);
        for &idx in face {
            normals[idx as usize] += normal;
        }
    }
    let mut bytes = Vec::new();
    for (position, normal) in positions.into_iter().zip(normals) {
        Vertex {
            position,
            normal: normal.try_normalize().unwrap_or(DVec3::Z),
            ..Vertex::default()
        }
        .write(layout, [0.0; 4], &mut bytes)?;
    }
    Ok(Output {
        geometry: Geometry::new(&bytes, layout, IndexBuffer::new(&indices)?)?,
        cells: cells.len(),
    })
}
