use {
    brotli::Decompressor,
    serde::{Deserialize, Serialize},
    snap::read::FrameDecoder,
    std::io::Read,
};

#[cfg(feature = "bake")]
use {brotli::CompressorWriter, snap::write::FrameEncoder, std::io::Write};

/// Describes Brotli-based compression.
#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub struct BrotliParams {
    /// Buffer size.
    pub buffer_size: usize,
    /// Compression quality.
    pub quality: u32,
    /// Window size.
    pub window_size: u32,
}

impl Default for BrotliParams {
    fn default() -> Self {
        Self {
            buffer_size: 4096,
            quality: 8,
            window_size: 22,
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize)]
pub enum Compression {
    Brotli(BrotliParams),
    Snap,
}

impl Compression {
    pub fn new_reader<'a>(self, reader: impl Read + 'a) -> Box<dyn Read + 'a> {
        match self {
            Compression::Brotli(b) => Box::new(Decompressor::new(reader, b.buffer_size)),
            Compression::Snap => Box::new(FrameDecoder::new(reader)),
        }
    }

    #[cfg(feature = "bake")]
    pub fn new_writer<'a>(self, writer: impl Write + 'a) -> Box<dyn Write + 'a> {
        match self {
            Compression::Brotli(b) => Box::new(CompressorWriter::new(
                writer,
                b.buffer_size,
                b.quality,
                b.window_size,
            )),
            Compression::Snap => Box::new(FrameEncoder::new(writer)),
        }
    }
}

impl Default for Compression {
    fn default() -> Self {
        Self::Brotli(Default::default())
    }
}

#[cfg(all(test, feature = "bake"))]
mod tests {
    use {
        super::{BrotliParams, Compression},
        std::io::{Read, Write},
    };

    #[test]
    fn brotli_round_trip() {
        let compression = Compression::Brotli(BrotliParams::default());
        let input = b"lossless compression round trip".repeat(32);
        let mut compressed = Vec::new();

        {
            let mut writer = compression.new_writer(&mut compressed);
            writer.write_all(&input).unwrap();
        }

        let mut output = Vec::new();
        compression
            .new_reader(compressed.as_slice())
            .read_to_end(&mut output)
            .unwrap();

        assert_eq!(output, input);
    }

    #[test]
    fn snap_round_trip() {
        let compression = Compression::Snap;
        let input = b"lossless compression round trip".repeat(32);
        let mut compressed = Vec::new();

        {
            let mut writer = compression.new_writer(&mut compressed);
            writer.write_all(&input).unwrap();
        }

        let mut output = Vec::new();
        compression
            .new_reader(compressed.as_slice())
            .read_to_end(&mut output)
            .unwrap();

        assert_eq!(output, input);
    }
}
