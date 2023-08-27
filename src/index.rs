use serde::{de::DeserializeOwned, Deserialize, Serialize};

#[derive(Clone, Debug, Deserialize, Serialize)]
pub struct IndexBuffer {
    #[serde(with = "serde_bytes")]
    buf: Vec<u8>,

    ty: IndexType,
}

impl IndexBuffer {
    pub fn new(indices: &[u32]) -> Self {
        debug_assert!(indices.len() >= 3);
        debug_assert_eq!(indices.len() % 3, 0);

        let max_vertex = indices.iter().copied().max().unwrap_or_default();

        debug_assert!(max_vertex <= u32::MAX as _);

        if max_vertex <= u8::MAX as _ {
            let mut buf = Vec::with_capacity(indices.len() << 1);
            for &idx in indices {
                buf.push(idx as u8);
            }

            Self {
                buf,
                ty: IndexType::U8,
            }
        } else if max_vertex <= u16::MAX as _ {
            let mut buf = Vec::with_capacity(indices.len() << 1);
            for &idx in indices {
                buf.extend_from_slice(&(idx as u16).to_ne_bytes());
            }

            Self {
                buf,
                ty: IndexType::U16,
            }
        } else {
            let mut buf = Vec::with_capacity(indices.len() << 2);
            for &idx in indices {
                buf.extend_from_slice(&idx.to_ne_bytes());
            }

            Self {
                buf,
                ty: IndexType::U32,
            }
        }
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

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub enum IndexType {
    U8,
    U16,
    U32,
}

#[cfg(test)]
mod tests {
    use crate::index::{IndexBuffer, IndexType};

    #[test]
    fn index_buffer_u8() {
        let index_buf = IndexBuffer::new(&[0, 1, 2]);

        assert_eq!(index_buf.triangle_count(), 1);
        assert_eq!(index_buf.index_count(), 3);
        assert_eq!(index_buf.index_type(), IndexType::U8);

        let buf = index_buf.as_u8().unwrap();

        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 1);
        assert_eq!(buf[2], 2);

        let buf = index_buf.as_u16().unwrap();

        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 1);
        assert_eq!(buf[2], 2);

        let buf = index_buf.as_u32();

        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 1);
        assert_eq!(buf[2], 2);
    }

    #[test]
    fn index_buffer_u16() {
        let index_buf = IndexBuffer::new(&[0, 1, 42_000]);

        assert_eq!(index_buf.triangle_count(), 1);
        assert_eq!(index_buf.index_count(), 3);
        assert_eq!(index_buf.index_type(), IndexType::U16);
        assert_eq!(index_buf.as_u8(), None);

        let buf = index_buf.as_u16().unwrap();

        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 1);
        assert_eq!(buf[2], 42_000);

        let buf = index_buf.as_u32();

        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 1);
        assert_eq!(buf[2], 42_000);
    }

    #[test]
    fn index_buffer_u32() {
        let index_buf = IndexBuffer::new(&[0, 1, 100_000]);

        assert_eq!(index_buf.triangle_count(), 1);
        assert_eq!(index_buf.index_count(), 3);
        assert_eq!(index_buf.index_type(), IndexType::U32);
        assert_eq!(index_buf.as_u8(), None);
        assert_eq!(index_buf.as_u16(), None);

        let buf = index_buf.as_u32();

        assert_eq!(buf.len(), 3);
        assert_eq!(buf[0], 0);
        assert_eq!(buf[1], 1);
        assert_eq!(buf[2], 100_000);
    }
}
