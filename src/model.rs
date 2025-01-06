use {
    super::{index::IndexBuffer, Mat4},
    crate::BlobId,
    bitflags::bitflags,
    serde::{Deserialize, Serialize},
};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Joint {
    /// A matrix which transform the mesh into the local space of the joint.
    pub inverse_bind: Mat4,

    /// Name of the joint/bone.
    pub name: String,

    /// Index into the skin joints to the parent of this joint.
    pub parent_index: usize,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct Mesh {
    name: Option<String>,
    parts: Vec<MeshPart>,
    skin: Option<Skin>,
}

impl Mesh {
    pub(super) fn new(name: Option<String>, parts: Vec<MeshPart>, skin: Option<Skin>) -> Self {
        Self { name, parts, skin }
    }

    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    pub fn parts(&self) -> &[MeshPart] {
        &self.parts
    }

    pub fn skin(&self) -> Option<&Skin> {
        self.skin.as_ref()
    }
}

#[derive(Clone, Debug, Default, Deserialize, Serialize)]
pub struct Model {
    data: Option<BlobId>,
    meshes: Vec<Mesh>,
}

impl Model {
    pub fn data(&self) -> Option<BlobId> {
        self.data
    }

    pub fn meshes(&self) -> &[Mesh] {
        &self.meshes
    }

    pub fn push_mesh(&mut self, mesh: Mesh) {
        self.meshes.push(mesh);
        self.meshes.sort_by(|lhs, rhs| lhs.name().cmp(&rhs.name()));
    }

    pub fn set_data(&mut self, id: BlobId) {
        self.data = Some(id);
    }
}

#[derive(Clone ,Debug, Deserialize, Serialize)]
pub struct MeshPart {
    lods: Vec<IndexBuffer>,
    material: u8,

    #[serde(with = "serde_bytes")]
    vertex_buf: Vec<u8>,

    vertex_type: VertexType,
}

impl MeshPart {
    pub fn new(material: u8, vertex_buf: &[u8], vertex_type: VertexType) -> Self {
        let vertex_buf = vertex_buf.to_vec();

        let res = Self {
            lods: Default::default(),
            material,
            vertex_buf,
            vertex_type,
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

    pub fn vertex_count(&self) -> usize {
        let stride = self.vertex_type.stride();
        let buf_len = self.vertex_buf.len();

        debug_assert_eq!(buf_len % stride, 0);

        buf_len / stride
    }

    pub fn vertex_data(&self) -> &[u8] {
        &self.vertex_buf
    }

    pub fn vertex_type(&self) -> VertexType {
        self.vertex_type
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
    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    pub struct VertexType: u8 {
        const POSITION = 1 << 0;
        const JOINTS_WEIGHTS = Self::POSITION.bits() | 1 << 1;
        const NORMAL = Self::POSITION.bits() | 1 << 2;
        const TANGENT = Self::POSITION.bits() | 1 << 3;
        const TEXTURE0 = Self::POSITION.bits() | 1 << 4;
        const TEXTURE1 = Self::TEXTURE0.bits() | 1 << 5;
    }
}

impl VertexType {
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
