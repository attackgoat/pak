use serde::{Deserialize, Deserializer, Serialize, de::Error};

/// Holds a `Bitmap` in a `.pak` file. For data transport only.
#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct Bitmap {
    color: BitmapColor,
    fmt: BitmapFormat,
    mip_levels: u32,

    #[serde(with = "serde_bytes")]
    pixels: Vec<u8>,

    width: u32,
    height: u32,

    #[serde(skip)]
    compressed: Option<CompressedBitmap>,
}

impl<'de> Deserialize<'de> for Bitmap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct BitmapData {
            color: BitmapColor,
            fmt: BitmapFormat,
            mip_levels: u32,

            #[serde(with = "serde_bytes")]
            pixels: Vec<u8>,

            width: u32,
            height: u32,
        }

        let data = BitmapData::deserialize(deserializer)?;
        if data.width == 0 {
            return Err(D::Error::custom("bitmap width must be greater than zero"));
        }

        if data.height == 0 {
            return Err(D::Error::custom("bitmap height must be greater than zero"));
        }

        let expected_len = data.width as usize * data.height as usize * data.fmt.byte_len();
        if data.pixels.len() != expected_len {
            return Err(D::Error::custom(
                "bitmap pixel byte length does not match its extent and format",
            ));
        }
        Ok(Self {
            color: data.color,
            fmt: data.fmt,
            mip_levels: data.mip_levels,
            pixels: data.pixels,
            width: data.width,
            height: data.height,
            compressed: None,
        })
    }
}

impl Bitmap {
    /// Pixel data must be tightly packed (no additional stride)
    pub fn new(
        color: BitmapColor,
        fmt: BitmapFormat,
        width: u32,
        mip_levels: u32,
        pixels: impl Into<Vec<u8>>,
    ) -> Self {
        let pixels = pixels.into();
        assert!(width > 0);
        assert_eq!(pixels.len() % (width as usize * fmt.byte_len()), 0);
        let height = (pixels.len() / (width as usize * fmt.byte_len())) as u32;

        assert!(height > 0);

        Self {
            color,
            fmt,
            mip_levels,
            pixels,
            width,
            height,
            compressed: None,
        }
    }

    #[cfg_attr(not(feature = "bake"), allow(dead_code))]
    pub(crate) fn with_compressed(mut self, compressed: CompressedBitmap) -> Self {
        compressed
            .validate(self.width, self.height, self.mip_levels)
            .expect("compressed bitmap data must be valid");
        self.compressed = Some(compressed);
        self
    }

    #[cfg_attr(not(feature = "bake"), allow(dead_code))]
    pub(crate) fn into_variants(mut self) -> (Self, Option<CompressedBitmap>) {
        let compressed = self.compressed.take();
        (self, compressed)
    }

    pub fn color(&self) -> BitmapColor {
        self.color
    }

    /// Returns a block-compressed variant attached before pak storage.
    ///
    /// Raw bitmaps read from a pak do not load or contain this variant. Use
    /// `PakBuf::read_compressed_bitmap_id` to read its separate payload.
    pub fn compressed(&self) -> Option<&CompressedBitmap> {
        self.compressed.as_ref()
    }

    /// Gets the dimensions, in pixels, of this `Bitmap`.
    pub fn extent(&self) -> (u32, u32) {
        (self.width(), self.height())
    }

    // TODO: Maybe better naming.. Channels?
    /// Gets a description of the number of channels contained in this `Bitmap`.
    pub fn format(&self) -> BitmapFormat {
        self.fmt
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn mip_levels(&self) -> u32 {
        self.mip_levels
    }

    pub fn pixel(&self, x: u32, y: u32) -> &[u8] {
        assert!(x < self.width);
        assert!(y < self.height());

        let offset = y as usize * self.stride() + x as usize * self.fmt.byte_len();
        &self.pixels[offset..offset + self.fmt.byte_len()]
    }

    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    pub fn pixels_as_format(&self, dst_fmt: BitmapFormat) -> impl Iterator<Item = u8> + '_ {
        let stride = self.fmt.byte_len().min(dst_fmt.byte_len());
        self.pixels
            .chunks(self.fmt.byte_len())
            .flat_map(move |src| {
                let mut dst = [0; 4];
                dst[0..stride].copy_from_slice(&src[0..stride]);
                dst.into_iter().take(dst_fmt.byte_len())
            })
    }

    /// Bytes per row of pixels (there is no padding)
    pub fn stride(&self) -> usize {
        self.width as usize * self.fmt.byte_len()
    }

    pub fn width(&self) -> u32 {
        self.width
    }
}

/// Header-resident information about a logical bitmap and its stored variants.
#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct BitmapInfo {
    color: BitmapColor,
    format: BitmapFormat,
    compression: Option<BitmapCompression>,
    width: u32,
    height: u32,
    mip_levels: u32,
}

impl BitmapInfo {
    #[cfg_attr(not(feature = "bake"), allow(dead_code))]
    pub(crate) fn new(bitmap: &Bitmap) -> Self {
        Self {
            color: bitmap.color(),
            format: bitmap.format(),
            compression: bitmap.compressed().map(CompressedBitmap::format),
            width: bitmap.width(),
            height: bitmap.height(),
            mip_levels: bitmap.mip_levels(),
        }
    }

    pub fn color(self) -> BitmapColor {
        self.color
    }

    pub fn format(self) -> BitmapFormat {
        self.format
    }

    pub fn compression(self) -> Option<BitmapCompression> {
        self.compression
    }

    pub fn has_compressed(self) -> bool {
        self.compression.is_some()
    }

    pub fn extent(self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn width(self) -> u32 {
        self.width
    }

    pub fn height(self) -> u32 {
        self.height
    }

    pub fn mip_levels(self) -> u32 {
        self.mip_levels
    }

    pub(crate) fn matches_raw(self, bitmap: &Bitmap) -> bool {
        self.color == bitmap.color()
            && self.format == bitmap.format()
            && self.extent() == bitmap.extent()
            && self.mip_levels == bitmap.mip_levels()
    }
}

/// GPU block-compression format for a baked bitmap variant.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
#[serde(rename_all = "kebab-case")]
pub enum BitmapCompression {
    Bc1Rgb,
    Bc1Srgb,
    Bc3,
    Bc4,
    Bc5,
}

impl BitmapCompression {
    /// Returns the number of bytes in one 4x4 block.
    pub const fn block_byte_len(self) -> usize {
        match self {
            Self::Bc1Rgb | Self::Bc1Srgb | Self::Bc4 => 8,
            Self::Bc3 | Self::Bc5 => 16,
        }
    }

    fn mip_byte_len(self, width: u32, height: u32) -> usize {
        width.div_ceil(4) as usize * height.div_ceil(4) as usize * self.block_byte_len()
    }
}

/// A block-compressed bitmap payload containing a complete mip chain.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompressedBitmap {
    format: BitmapCompression,
    mips: Vec<CompressedMip>,
}

impl CompressedBitmap {
    #[cfg_attr(not(feature = "bake"), allow(dead_code))]
    pub(crate) fn new(format: BitmapCompression, mips: Vec<CompressedMip>) -> Self {
        Self { format, mips }
    }

    pub fn format(&self) -> BitmapCompression {
        self.format
    }

    pub fn mips(&self) -> &[CompressedMip] {
        &self.mips
    }

    pub fn extent(&self) -> (u32, u32) {
        self.mips.first().map_or((0, 0), CompressedMip::extent)
    }

    pub fn width(&self) -> u32 {
        self.extent().0
    }

    pub fn height(&self) -> u32 {
        self.extent().1
    }

    pub fn mip_levels(&self) -> u32 {
        self.mips.len() as u32
    }

    pub(crate) fn validate(
        &self,
        width: u32,
        height: u32,
        mip_levels: u32,
    ) -> Result<(), &'static str> {
        if self.mips.len() != mip_levels as usize {
            return Err("compressed bitmap mip count does not match mip_levels");
        }

        let (mut expected_width, mut expected_height) = (width, height);
        for mip in &self.mips {
            if mip.extent() != (expected_width, expected_height) {
                return Err("compressed bitmap mip extent is invalid");
            }
            if mip.bytes.len() != self.format.mip_byte_len(mip.width, mip.height) {
                return Err("compressed bitmap mip byte length is invalid");
            }
            expected_width = (expected_width / 2).max(1);
            expected_height = (expected_height / 2).max(1);
        }

        Ok(())
    }
}

/// One level of a block-compressed bitmap variant.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct CompressedMip {
    width: u32,
    height: u32,

    #[serde(with = "serde_bytes")]
    bytes: Vec<u8>,
}

impl CompressedMip {
    #[cfg_attr(not(feature = "bake"), allow(dead_code))]
    pub(crate) fn new(width: u32, height: u32, bytes: Vec<u8>) -> Self {
        Self {
            width,
            height,
            bytes,
        }
    }

    pub fn extent(&self) -> (u32, u32) {
        (self.width, self.height)
    }

    pub fn width(&self) -> u32 {
        self.width
    }

    pub fn height(&self) -> u32 {
        self.height
    }

    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[cfg(test)]
mod test {
    use crate::bitmap::{
        Bitmap, BitmapColor, BitmapCompression, BitmapFormat, CompressedBitmap, CompressedMip,
    };

    #[test]
    fn pixels_as_format_drops_extra_channels() {
        let bitmap = Bitmap::new(
            BitmapColor::Srgb,
            BitmapFormat::Rgba,
            2,
            1,
            [1, 2, 3, 4, 5, 6, 7, 8],
        );

        let pixels = bitmap
            .pixels_as_format(BitmapFormat::Rgb)
            .collect::<Vec<_>>();

        assert_eq!(pixels, [1, 2, 3, 5, 6, 7]);
    }

    #[test]
    fn pixels_as_format_pads_missing_channels() {
        let bitmap = Bitmap::new(BitmapColor::Srgb, BitmapFormat::R, 2, 1, [1, 2]);

        let pixels = bitmap
            .pixels_as_format(BitmapFormat::Rgb)
            .collect::<Vec<_>>();

        assert_eq!(pixels, [1, 0, 0, 2, 0, 0]);
    }

    #[test]
    #[should_panic]
    fn pixel_rejects_x_past_width() {
        let bitmap = Bitmap::new(BitmapColor::Srgb, BitmapFormat::R, 2, 1, [1, 2, 3, 4]);

        let _ = bitmap.pixel(2, 0);
    }

    #[test]
    #[should_panic]
    fn new_rejects_zero_width() {
        let _ = Bitmap::new(BitmapColor::Srgb, BitmapFormat::R, 0, 1, [1, 2]);
    }

    #[test]
    #[should_panic]
    fn new_rejects_partial_rows() {
        let _ = Bitmap::new(BitmapColor::Srgb, BitmapFormat::Rgb, 1, 1, [1, 2, 3, 4]);
    }

    #[test]
    fn deserialize_rejects_zero_width() {
        let invalid = Bitmap {
            color: BitmapColor::Srgb,
            fmt: BitmapFormat::R,
            mip_levels: 1,
            pixels: vec![1],
            width: 0,
            height: 1,
            compressed: None,
        };
        let mut encoded = Vec::new();
        bincode::serde::encode_into_std_write(invalid, &mut encoded, bincode::config::legacy())
            .unwrap();

        let result =
            bincode::serde::decode_from_slice::<Bitmap, _>(&encoded, bincode::config::legacy());

        assert!(result.is_err());
    }

    #[test]
    fn raw_bitmap_serialization_omits_transient_compressed_variant() {
        let bitmap = Bitmap::new(
            BitmapColor::Srgb,
            BitmapFormat::Rgba,
            5,
            3,
            vec![42; 5 * 3 * 4],
        )
        .with_compressed(CompressedBitmap::new(
            BitmapCompression::Bc1Srgb,
            vec![
                CompressedMip::new(5, 3, vec![1; 16]),
                CompressedMip::new(2, 1, vec![2; 8]),
                CompressedMip::new(1, 1, vec![3; 8]),
            ],
        ));
        let encoded = bincode::serde::encode_to_vec(&bitmap, bincode::config::legacy()).unwrap();
        let (decoded, consumed) =
            bincode::serde::decode_from_slice::<Bitmap, _>(&encoded, bincode::config::legacy())
                .unwrap();

        assert_eq!(consumed, encoded.len());
        assert_eq!(decoded.extent(), (5, 3));
        assert_eq!(decoded.pixels(), vec![42; 5 * 3 * 4]);
        assert!(decoded.compressed().is_none());
        assert!(bitmap.compressed().is_some());
    }
}

/// Describes the channels of a `Bitmap`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum BitmapFormat {
    /// Red channel only.
    #[serde(rename = "r")]
    R,

    /// Red and green channels.
    #[serde(rename = "rg")]
    Rg,

    /// Red, green and blue channels.
    #[serde(rename = "rgb")]
    Rgb,

    /// Red, green, blue and alpha channels.
    #[serde(rename = "rgba")]
    Rgba,
}

impl BitmapFormat {
    /// Returns the number of bytes each pixel advances the bitmap stream.
    #[inline]
    pub const fn byte_len(self) -> usize {
        match self {
            Self::R => 1,
            Self::Rg => 2,
            Self::Rgb => 3,
            Self::Rgba => 4,
        }
    }

    /// Returns `true` if this format includes an alpha channel.
    pub const fn has_alpha(self) -> bool {
        matches!(self, Self::Rgba)
    }
}

/// Describes the color space of a `Bitmap`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq, Serialize)]
pub enum BitmapColor {
    #[serde(rename = "linear")]
    Linear,

    #[serde(rename = "srgb")]
    Srgb,
}
