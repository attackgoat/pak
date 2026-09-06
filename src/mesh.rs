use {
    super::{Mat4, index::IndexBuffer},
    crate::{
        BlobId,
        scene::{DataMap, DataRef},
    },
    bitflags::bitflags,
    serde::{Deserialize, Deserializer, Serialize, de::Error},
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
    blob: Option<BlobId>,
    pub data: DataMap,
    primitives: Vec<Primitive>,
    skin: Option<Skin>,
}

impl Mesh {
    #[cfg(feature = "bake")]
    pub(super) fn new(primitives: Vec<Primitive>, skin: Option<Skin>) -> Self {
        Self {
            blob: None,
            data: DataMap::default(),
            primitives,
            skin,
        }
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

    pub fn set_blob(&mut self, id: BlobId) {
        self.blob = Some(id);
    }

    pub fn skin(&self) -> Option<&Skin> {
        self.skin.as_ref()
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct Primitive {
    lods: Vec<IndexBuffer>,
    material: u8,

    #[serde(with = "serde_bytes")]
    vertex_buf: Vec<u8>,

    vertex_type: VertexType,
}

impl<'de> Deserialize<'de> for Primitive {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct PrimitiveData {
            lods: Vec<IndexBuffer>,
            material: u8,

            #[serde(with = "serde_bytes")]
            vertex_buf: Vec<u8>,

            vertex_type: VertexType,
        }

        let data = PrimitiveData::deserialize(deserializer)?;
        if !data.vertex_type.contains(VertexType::POSITION) {
            return Err(D::Error::custom(
                "primitive vertex type must include positions",
            ));
        }

        if data.vertex_buf.is_empty() {
            return Err(D::Error::custom(
                "primitive vertex buffer must not be empty",
            ));
        }

        if !data
            .vertex_buf
            .len()
            .is_multiple_of(data.vertex_type.stride())
        {
            return Err(D::Error::custom(
                "primitive vertex buffer byte length is malformed",
            ));
        }

        Ok(Self {
            lods: data.lods,
            material: data.material,
            vertex_buf: data.vertex_buf,
            vertex_type: data.vertex_type,
        })
    }
}

impl Primitive {
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

    pub fn push_lod(&mut self, indices: IndexBuffer) {
        self.lods.push(indices);
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

    /// Returns the first texture coordinate for each vertex, when present.
    pub fn texture0(&self) -> Option<impl ExactSizeIterator<Item = [f32; 2]> + '_> {
        if !self.vertex_type.contains(VertexType::TEXTURE0) {
            return None;
        }

        let offset = 12
            + if self.vertex_type.contains(VertexType::NORMAL) {
                12
            } else {
                0
            };

        Some(
            self.vertex_buf
                .chunks_exact(self.vertex_type.stride())
                .map(move |vertex| {
                    let u = f32::from_ne_bytes(vertex[offset..offset + 4].try_into().unwrap());
                    let v = f32::from_ne_bytes(vertex[offset + 4..offset + 8].try_into().unwrap());
                    [u, v]
                }),
        )
    }

    pub fn vertex_type(&self) -> VertexType {
        self.vertex_type
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

bitflags! {
    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    pub struct VertexType: u8 {
        const POSITION = 1 << 0;
        const JOINTS_WEIGHTS = Self::POSITION.bits() | (1 << 1);
        const NORMAL = Self::POSITION.bits() | (1 << 2);
        const TANGENT = Self::POSITION.bits() | (1 << 3);
        const TEXTURE0 = Self::POSITION.bits() | (1 << 4);
        const TEXTURE1 = Self::POSITION.bits() | (1 << 5);
    }
}

impl VertexType {
    pub fn stride(&self) -> usize {
        let mut res = 12;

        debug_assert!(self.contains(Self::POSITION));

        if self.contains(Self::NORMAL) {
            res += 12;
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
}

#[cfg(test)]
mod test {
    use super::{Primitive, VertexType};

    #[test]
    fn deserialize_rejects_malformed_primitive_vertex_buffer() {
        let invalid = Primitive {
            lods: Vec::new(),
            material: 0,
            vertex_buf: vec![0],
            vertex_type: VertexType::POSITION,
        };
        let mut encoded = Vec::new();
        bincode::serde::encode_into_std_write(invalid, &mut encoded, bincode::config::legacy())
            .unwrap();

        let result =
            bincode::serde::decode_from_slice::<Primitive, _>(&encoded, bincode::config::legacy());

        assert!(result.is_err());
    }

    #[test]
    fn texture0_reads_interleaved_vertices() {
        let vertex_type = VertexType::POSITION | VertexType::NORMAL | VertexType::TEXTURE0;
        let mut vertices = Vec::new();
        for texture in [[0.25_f32, 0.5_f32], [0.75, 1.0]] {
            vertices.extend_from_slice(&[0; 24]);
            vertices.extend_from_slice(&texture[0].to_ne_bytes());
            vertices.extend_from_slice(&texture[1].to_ne_bytes());
        }

        let primitive = Primitive::new(0, &vertices, vertex_type);

        assert_eq!(
            primitive.texture0().unwrap().collect::<Vec<_>>(),
            [[0.25, 0.5], [0.75, 1.0]]
        );
    }

    #[test]
    fn texture0_is_absent_when_not_in_vertex_type() {
        let primitive = Primitive::new(0, &[0; 12], VertexType::POSITION);

        assert!(primitive.texture0().is_none());
    }
}
