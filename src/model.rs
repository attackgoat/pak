use {
    bitflags::bitflags,
    glam::Mat4,
    serde::{Deserialize, Serialize},
    std::collections::HashMap,
};

// #[derive(Debug, Deserialize, Serialize)]
// pub struct Joint {
//     children: Vec<Self>,
//     inverse_bind: Mat4,
//     name: String,
//     transform: Mat4,
// }

// impl Joint {
//     pub(super) fn new(name: String, inverse_bind: Mat4, transform: Mat4) -> Self {
//         Self {
//             children: vec![],
//             inverse_bind,
//             name,
//             transform,
//         }
//     }

//     pub fn children(&self) -> &[Self] {
//         &self.children
//     }

//     pub fn inverse_bind(&self) -> Mat4 {
//         self.inverse_bind
//     }

//     pub fn name(&self) -> &str {
//         &self.name
//     }

//     pub(super) fn push_child(&mut self, child: Self) {
//         self.children.push(child);
//     }

//     pub fn transform(&self) -> Mat4 {
//         self.transform
//     }
// }

#[derive(Debug, Deserialize, Serialize)]
enum Index {
    U8,
    U16,
    U32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct IndexBuffer {
    #[serde(with = "serde_bytes")]
    buf: Vec<u8>,

    ty: Index,
}

impl IndexBuffer {
    fn new(indices: &[u32]) -> Self {
        debug_assert!(indices.len() >= 3);
        debug_assert_eq!(indices.len() % 3, 0);

        let max_vertex = indices.iter().copied().max().unwrap_or_default();

        debug_assert!(max_vertex <= u32::MAX as _);

        if max_vertex <= u8::MAX as _ {
            let mut buf = Vec::with_capacity(indices.len() << 1);
            for &idx in indices {
                buf.push(idx as u8);
            }

            Self { buf, ty: Index::U8 }
        } else if max_vertex <= u16::MAX as _ {
            let mut buf = Vec::with_capacity(indices.len() << 1);
            for &idx in indices {
                buf.extend_from_slice(&(idx as u16).to_ne_bytes());
            }

            Self {
                buf,
                ty: Index::U16,
            }
        } else {
            let mut buf = Vec::with_capacity(indices.len() << 2);
            for &idx in indices {
                buf.extend_from_slice(&idx.to_ne_bytes());
            }

            Self {
                buf,
                ty: Index::U32,
            }
        }
    }

    pub fn index_buffer(&self) -> Vec<u32> {
        match self.ty {
            Index::U8 => self.buf.iter().copied().map(|idx| idx as _).collect(),
            Index::U16 => {
                debug_assert_eq!(self.buf.len() % 2, 0);

                let count = self.buf.len() >> 1;
                let mut res = Vec::with_capacity(count);
                for idx in 0..count {
                    let idx = idx << 1;
                    let data = &self.buf[idx..idx + 2];
                    res.push(u16::from_ne_bytes([data[0], data[1]]) as _);
                }

                res
            }
            Index::U32 => {
                debug_assert_eq!(self.buf.len() % 4, 0);

                let count = self.buf.len() >> 2;
                let mut res = Vec::with_capacity(count);
                for idx in 0..count {
                    let idx = idx << 2;
                    let data = &self.buf[idx..idx + 4];
                    res.push(u32::from_ne_bytes([data[0], data[1], data[2], data[3]]));
                }

                res
            }
        }
    }

    pub fn index_count(&self) -> usize {
        match self.ty {
            Index::U8 => self.buf.len(),
            Index::U16 => self.buf.len() >> 1,
            Index::U32 => self.buf.len() >> 2,
        }
    }

    pub fn triangle_count(&self) -> usize {
        self.index_count() / 3
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Joint {
    pub inverse_bind: Mat4,
    pub name: String,
    pub parent_index: usize,
    pub transform: Mat4,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Mesh {
    name: Option<String>,
    primitives: Vec<Primitive>,
    skin: Option<Skin>,
}

impl Mesh {
    pub(super) fn new(
        name: Option<String>,
        primitives: Vec<Primitive>,
        skin: Option<Skin>,
    ) -> Self {
        Self {
            name,
            primitives,
            skin,
        }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn primitives(&self) -> &[Primitive] {
        &self.primitives
    }

    pub fn skin(&self) -> Option<&Skin> {
        self.skin.as_ref()
    }
}

#[derive(Debug, Default, Deserialize, Serialize)]
pub struct ModelBuf {
    meshes: Vec<Mesh>,
}

impl ModelBuf {
    pub fn meshes(&self) -> &[Mesh] {
        &self.meshes
    }

    pub fn push_mesh(&mut self, mesh: Mesh) {
        self.meshes.push(mesh);
        self.meshes.sort_by(|lhs, rhs| lhs.name().cmp(&rhs.name()));
    }
}

#[derive(Debug, Deserialize, Serialize)]
pub struct Primitive {
    lods: Vec<IndexBuffer>,
    material: u8,

    #[serde(with = "serde_bytes")]
    vertex_buf: Vec<u8>,

    vertex_ty: Vertex,
}

impl Primitive {
    pub fn new(material: u8, vertex_buf: &[u8], vertex_ty: Vertex) -> Self {
        let res = Self {
            lods: Default::default(),
            material,
            vertex_buf: vertex_buf.to_vec(),
            vertex_ty,
        };

        debug_assert!(res.vertex_count() > 0);

        res
    }

    pub fn lods(&self) -> &[IndexBuffer] {
        &self.lods
    }

    pub fn material(&self) -> u8 {
        self.material
    }

    pub fn push_lod(&mut self, indices: &[u32]) {
        self.lods.push(IndexBuffer::new(indices));
    }

    pub fn vertex(&self) -> Vertex {
        self.vertex_ty
    }

    pub fn vertex_data(&self) -> &[u8] {
        &self.vertex_buf
    }

    pub fn vertex_count(&self) -> usize {
        let stride = self.vertex_ty.stride();
        let buf_len = self.vertex_buf.len();

        debug_assert_eq!(buf_len % stride, 0);

        buf_len / stride
    }
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Skin {
    joints: Vec<Joint>,
}

impl Skin {
    pub(super) fn new(joints: Vec<Joint>) -> Self {
        Self { joints }
    }

    pub fn joints(&self) -> &[Joint] {
        &self.joints
    }
}

bitflags! {
    #[derive(Deserialize, Serialize)]
    pub struct Vertex: u8 {
        const POSITION = 1 << 0;
        const JOINTS_WEIGHTS = Self::POSITION.bits() | 1 << 1;
        const NORMAL = Self::POSITION.bits() | 1 << 2;
        const TANGENT = Self::POSITION.bits() | 1 << 3;
        const TEXTURE0 = Self::POSITION.bits() | 1 << 4;
        const TEXTURE1 = Self::TEXTURE0.bits() | 1 << 5;
    }
}

impl Vertex {
    pub fn stride(&self) -> usize {
        let mut res = 12;

        debug_assert!(self.contains(Self::POSITION));

        if self.contains(Self::JOINTS_WEIGHTS) {
            res += 8;
        }

        if self.contains(Self::NORMAL) {
            res += 12;
        }

        if self.contains(Self::TANGENT) {
            res += 16;
        }

        if self.contains(Self::TEXTURE0) {
            res += 8;
        }

        if self.contains(Self::TEXTURE1) {
            res += 8;
        }

        res
    }
}

#[cfg(test)]
mod tests {
    use crate::model::IndexBuffer;

    #[test]
    fn index_buffer_u8() {
        let buf = IndexBuffer::new(&[0, 1, 2]);

        assert_eq!(buf.triangle_count(), 1);
        assert_eq!(buf.index_count(), 3);

        let buf = buf.index_buffer();

        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 1);
        assert_eq!(buf[2], 2);
    }

    #[test]
    fn index_buffer_u16() {
        let buf = IndexBuffer::new(&[0, 1, 42_000]);

        assert_eq!(buf.triangle_count(), 1);
        assert_eq!(buf.index_count(), 3);

        let buf = buf.index_buffer();

        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 1);
        assert_eq!(buf[2], 42_000);
    }

    #[test]
    fn index_buffer_u32() {
        let buf = IndexBuffer::new(&[0, 1, 100_000]);

        assert_eq!(buf.triangle_count(), 1);
        assert_eq!(buf.index_count(), 3);

        let buf = buf.index_buffer();

        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 1);
        assert_eq!(buf[2], 100_000);
    }
}
