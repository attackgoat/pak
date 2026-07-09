use {
    anyhow::bail,
    serde::{Deserialize, Deserializer, Serialize, de::Error},
    std::mem::size_of,
};

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct IndexBuffer {
    buf: Vec<u8>,
    ty: IndexType,
}

impl IndexBuffer {
    pub fn new(indices: &[u32]) -> anyhow::Result<Self> {
        if indices.len() < 3 {
            bail!("index buffer must have at least 3 indices");
        }

        if !indices.len().is_multiple_of(3) {
            bail!("index buffer length must be a multiple of 3");
        }

        let max_vertex = indices.iter().copied().max().unwrap_or_default();

        let (buf, ty) = if max_vertex <= u8::MAX as _ {
            let mut buf = Vec::with_capacity(indices.len());
            for &idx in indices {
                buf.push(idx as u8);
            }

            (buf, IndexType::U8)
        } else if max_vertex <= u16::MAX as _ {
            let mut buf = Vec::with_capacity(indices.len() << 1);
            for &idx in indices {
                buf.extend_from_slice(&(idx as u16).to_ne_bytes());
            }

            (buf, IndexType::U16)
        } else {
            let mut buf = Vec::with_capacity(indices.len() << 2);
            for &idx in indices {
                buf.extend_from_slice(&idx.to_ne_bytes());
            }

            (buf, IndexType::U32)
        };

        Ok(Self { buf, ty })
    }

    pub fn as_u8(&self) -> Option<Vec<u8>> {
        match self.ty {
            IndexType::U8 => Some(self.buf.iter().copied().map(|idx| idx as _).collect()),
            _ => None,
        }
    }

    pub fn as_u16(&self) -> Option<Vec<u16>> {
        match self.ty {
            IndexType::U8 => Some(self.buf.iter().copied().map(|idx| idx as u16).collect()),
            IndexType::U16 => {
                debug_assert_eq!(self.buf.len() % 2, 0);

                let count = self.buf.len() >> 1;
                let mut res = Vec::with_capacity(count);
                for idx in 0..count {
                    let idx = idx << 1;
                    let data = &self.buf[idx..idx + 2];
                    res.push(u16::from_ne_bytes([data[0], data[1]]));
                }

                Some(res)
            }
            _ => None,
        }
    }

    pub fn as_u32(&self) -> Vec<u32> {
        match self.ty {
            IndexType::U8 => self.buf.iter().copied().map(|idx| idx as _).collect(),
            IndexType::U16 => {
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
            IndexType::U32 => {
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
            IndexType::U8 => self.buf.len(),
            IndexType::U16 => self.buf.len() >> 1,
            IndexType::U32 => self.buf.len() >> 2,
        }
    }

    pub fn index_type(&self) -> IndexType {
        self.ty
    }

    pub fn triangle_count(&self) -> usize {
        self.index_count() / 3
    }
}

impl<'de> Deserialize<'de> for IndexBuffer {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct IndexBufferData {
            buf: Vec<u8>,
            ty: IndexType,
        }

        let data = IndexBufferData::deserialize(deserializer)?;
        let stride = data.ty.stride();
        if data.buf.len() % stride != 0 {
            return Err(D::Error::custom("index buffer byte length is malformed"));
        }

        let index_count = data.buf.len() / stride;
        if index_count < 3 {
            return Err(D::Error::custom(
                "index buffer must have at least 3 indices",
            ));
        }

        if !index_count.is_multiple_of(3) {
            return Err(D::Error::custom(
                "index buffer length must be a multiple of 3",
            ));
        }

        Ok(Self {
            buf: data.buf,
            ty: data.ty,
        })
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum IndexType {
    U8,
    U16,
    U32,
}

impl IndexType {
    pub fn stride(self) -> usize {
        match self {
            Self::U8 => size_of::<u8>(),
            Self::U16 => size_of::<u16>(),
            Self::U32 => size_of::<u32>(),
        }
    }
}

#[cfg(test)]
mod tests {
    use crate::index::{IndexBuffer, IndexType};

    #[test]
    fn index_buffer_u8() {
        let indices = [0, 1, 2, 0, 1, 3];
        let index_buf = IndexBuffer::new(&indices);

        let index_buf = index_buf.expect("IndexBuffer::new should succeed for u8 indices");
        assert_eq!(index_buf.triangle_count(), 2);
        assert_eq!(index_buf.index_count(), 6);
        assert_eq!(index_buf.index_type(), IndexType::U8);

        let buf = index_buf.as_u8().expect("IndexBuffer should be u8");

        assert_eq!(buf.len(), 6);
        for (expected, actual) in indices.iter().copied().zip(buf) {
            assert_eq!(expected, actual as u32);
        }

        let buf = index_buf
            .as_u16()
            .expect("IndexBuffer should convert to u16");

        assert_eq!(buf.len(), 6);
        for (expected, actual) in indices.iter().copied().zip(buf) {
            assert_eq!(expected, actual as u32);
        }

        let buf = index_buf.as_u32();

        assert_eq!(buf.len(), 6);
        for (expected, actual) in indices.iter().copied().zip(buf) {
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn index_buffer_u16() {
        let indices = [0, 1, 42_000, 0, 1, 2];
        let index_buf = IndexBuffer::new(&indices).expect("IndexBuffer::new should succeed");

        assert_eq!(index_buf.triangle_count(), 2);
        assert_eq!(index_buf.index_count(), 6);
        assert_eq!(index_buf.index_type(), IndexType::U16);
        assert_eq!(index_buf.as_u8(), None);

        let buf = index_buf
            .as_u16()
            .expect("IndexBuffer should convert to u16");

        assert_eq!(buf.len(), 6);
        for (expected, actual) in indices.iter().copied().zip(buf) {
            assert_eq!(expected, actual as u32);
        }

        let buf = index_buf.as_u32();

        assert_eq!(buf.len(), 6);
        for (expected, actual) in indices.iter().copied().zip(buf) {
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn index_buffer_u32() {
        let indices = [0, 1, 100_000, 0, 1, 2];
        let index_buf = IndexBuffer::new(&indices).expect("IndexBuffer::new should succeed");

        assert_eq!(index_buf.triangle_count(), 2);
        assert_eq!(index_buf.index_count(), 6);
        assert_eq!(index_buf.index_type(), IndexType::U32);
        assert_eq!(index_buf.as_u8(), None);
        assert_eq!(index_buf.as_u16(), None);

        let buf = index_buf.as_u32();

        assert_eq!(buf.len(), 6);
        for (expected, actual) in indices.iter().copied().zip(buf) {
            assert_eq!(expected, actual);
        }
    }

    #[test]
    fn deserialize_rejects_malformed_index_buffer() {
        let invalid = IndexBuffer {
            buf: vec![0],
            ty: IndexType::U16,
        };
        let mut encoded = Vec::new();
        bincode::serde::encode_into_std_write(invalid, &mut encoded, bincode::config::legacy())
            .unwrap();

        let result = bincode::serde::decode_from_slice::<IndexBuffer, _>(
            &encoded,
            bincode::config::legacy(),
        );

        assert!(result.is_err());
    }
}
