use {
    super::{Canonicalize, Writer, file_key, re_run_if_changed},
    crate::{
        BitmapId,
        bitmap::{
            Bitmap, BitmapColor, BitmapCompression, BitmapFormat, CompressedBitmap, CompressedMip,
        },
    },
    anyhow::{Context, bail},
    image::{
        DynamicImage, RgbaImage,
        buffer::ConvertBuffer,
        imageops::{FilterType, resize},
        open,
    },
    log::{info, trace, warn},
    parking_lot::Mutex,
    rayon::prelude::*,
    serde::{Deserialize, Deserializer, de::Visitor},
    std::{
        fmt::Formatter,
        fs::{create_dir_all, read, remove_file, rename, write},
        path::{Path, PathBuf},
        sync::{
            Arc, OnceLock,
            atomic::{AtomicU64, Ordering},
        },
    },
};

const MIP_LEVELS_MAX: u32 = u32::BITS;
const MIP_LEVELS_MIN: u32 = 1;
const TEXTURE_CACHE_RECIPE: &[u8] = b"pak-bc-texture/v1";
static TEXTURE_CACHE_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn de_mip_levels<'de, D>(deserializer: D) -> Result<u32, D::Error>
where
    D: Deserializer<'de>,
{
    struct MipLevelsVisitor;

    impl Visitor<'_> for MipLevelsVisitor {
        type Value = Option<u32>;

        fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
            formatter.write_str("either boolean or an non-zero unsigned integer")
        }

        fn visit_bool<E>(self, v: bool) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            let mip_levels = if v { MIP_LEVELS_MAX } else { MIP_LEVELS_MIN };

            Ok(Some(mip_levels))
        }

        fn visit_i64<E>(self, v: i64) -> Result<Self::Value, E>
        where
            E: serde::de::Error,
        {
            if v > 0 {
                Ok(Some(v.min(MIP_LEVELS_MAX as _) as _))
            } else {
                Err(E::invalid_value(
                    serde::de::Unexpected::Unsigned(v as _),
                    &"a non-zero unsigned integer",
                ))
            }
        }
    }

    deserializer
        .deserialize_any(MipLevelsVisitor)
        .map(|res| res.unwrap_or(MIP_LEVELS_MIN))
}

fn default_mip_levels() -> u32 {
    MIP_LEVELS_MIN
}

/// Holds a description of `.jpeg` and other regular images.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
#[serde(rename_all = "kebab-case")]
pub struct BitmapAsset {
    color: Option<BitmapColor>,
    compression: Option<BitmapCompression>,

    #[serde(default = "default_mip_levels", deserialize_with = "de_mip_levels")]
    mip_levels: u32,

    resize: Option<u32>,
    src: Option<PathBuf>,

    /// Controls the number and order of the channels output into the final image
    #[serde(default, deserialize_with = "BitmapSwizzle::de")]
    swizzle: Option<BitmapSwizzle>,
}

impl BitmapAsset {
    /// Constructs a new Bitmap with the given image file source.
    pub fn new(src: impl AsRef<Path>) -> Self {
        Self {
            color: None,
            compression: None,
            mip_levels: 1,
            resize: None,
            src: Some(src.as_ref().to_path_buf()),
            swizzle: None,
        }
    }

    #[allow(dead_code)]
    pub fn with_color(mut self, color: BitmapColor) -> Self {
        self.color = Some(color);
        self
    }

    #[allow(dead_code)]
    pub fn with_mip_levels(mut self, mip_levels: u32) -> Self {
        self.mip_levels = mip_levels;
        self
    }

    #[allow(dead_code)]
    /// Enables a complete bake-time BC mip chain while retaining the raw base image.
    pub fn with_compression(mut self, compression: BitmapCompression) -> Self {
        self.compression = Some(compression);
        self
    }

    pub(super) fn with_default_compression(mut self, compression: BitmapCompression) -> Self {
        self.compression.get_or_insert(compression);
        self
    }

    pub(super) fn with_default_color_compression(self) -> Self {
        let compression = match self.color() {
            BitmapColor::Linear => BitmapCompression::Bc1Rgb,
            BitmapColor::Srgb => BitmapCompression::Bc1Srgb,
        };
        self.with_default_compression(compression)
    }

    pub(super) fn with_default_compression_if(
        self,
        enabled: bool,
        compression: BitmapCompression,
    ) -> Self {
        if enabled {
            self.with_default_compression(compression)
        } else {
            self
        }
    }

    pub(super) fn with_default_color_compression_if(self, enabled: bool) -> Self {
        if enabled {
            self.with_default_color_compression()
        } else {
            self
        }
    }

    #[allow(dead_code)]
    pub fn with_swizzle(mut self, swizzle: BitmapSwizzle) -> Self {
        self.swizzle = Some(swizzle);
        self
    }

    /// Reads and processes image source files into an existing `.pak` file buffer.
    pub fn bake(
        &mut self,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
    ) -> anyhow::Result<BitmapId> {
        self.bake_from_path(writer, project_dir, None as Option<&'static str>)
    }

    /// Reads and processes image source files into an existing `.pak` file buffer.
    pub fn bake_from_path(
        &mut self,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        path: Option<impl AsRef<Path>>,
    ) -> anyhow::Result<BitmapId> {
        let Some(src) = self.src() else {
            return Err(anyhow::Error::msg("unspecified bitmap source"));
        };

        // Early-out if we have already baked this bitmap
        let asset = self.clone().into();
        let key = path.as_ref().map(|path| file_key(&project_dir, path));
        if let Some(id) = writer.lock().asset_id(&asset, key.as_deref())? {
            return id
                .as_bitmap()
                .context("asset context returned non-bitmap id");
        }

        if let Some(key) = &key {
            // This bitmap will be accessible using this key
            info!("Baking bitmap: {}", key);
        } else {
            // This bitmap will only be accessible using the id
            info!("Baking bitmap: {} (inline)", file_key(&project_dir, src));
        }

        let bitmap = self
            .as_bitmap_buf()
            .context("Unable to create bitmap buf")?;

        let mut writer = writer.lock();
        if let Some(id) = writer.asset_id(&asset, key.as_deref())? {
            return id
                .as_bitmap()
                .context("asset context returned non-bitmap id");
        }

        let policy = writer.policy_for(&asset);
        let id = writer.push_bitmap(bitmap, policy)?;
        writer.commit_asset(asset, id, key)?;

        Ok(id)
    }

    pub fn as_bitmap_buf(&self) -> anyhow::Result<Bitmap> {
        let Some(src) = self.src() else {
            return Err(anyhow::Error::msg("unspecified bitmap source"));
        };

        let (format, width, pixels) =
            Self::read_pixels(src, self.swizzle, self.resize).context("Unable to read pixels")?;

        let row_length = format.byte_len() * width as usize;

        assert_eq!(pixels.len() % row_length, 0);

        let height = (pixels.len() / row_length) as u32;

        if width == 0 || height == 0 {
            bail!("invalid image size");
        }

        let mip_levels_max = u32::BITS - width.max(height).leading_zeros();
        let mip_levels = if self.compression.is_some() {
            mip_levels_max
        } else {
            self.mip_levels.clamp(MIP_LEVELS_MIN, mip_levels_max)
        };

        let bitmap = Bitmap::new(self.color(), format, width, mip_levels, pixels);

        Ok(if let Some(compression) = self.compression {
            let compression = if format.has_alpha()
                && matches!(
                    compression,
                    BitmapCompression::Bc1Rgb | BitmapCompression::Bc1Srgb
                ) {
                BitmapCompression::Bc3
            } else {
                compression
            };
            let compressed = Self::compress(&bitmap, compression);
            bitmap.with_compressed(compressed)
        } else {
            bitmap
        })
    }

    pub fn color(&self) -> BitmapColor {
        self.color.unwrap_or(BitmapColor::Srgb)
    }

    #[allow(dead_code)]
    pub fn compression(&self) -> Option<BitmapCompression> {
        self.compression
    }

    pub(super) fn compress(bitmap: &Bitmap, compression: BitmapCompression) -> CompressedBitmap {
        let algorithm = Self::compression_algorithm();
        let cache_key = Self::texture_cache_key(bitmap, compression, algorithm);
        if let Some(compressed) = Self::read_texture_cache(bitmap, compression, &cache_key) {
            trace!("BC texture cache hit: {cache_key}");
            return compressed;
        }

        let mut image = RgbaImage::from_fn(bitmap.width(), bitmap.height(), |x, y| {
            let src = bitmap.pixel(x, y);
            image::Rgba(match bitmap.format() {
                BitmapFormat::R => [src[0], 0, 0, u8::MAX],
                BitmapFormat::Rg => [src[0], src[1], 0, u8::MAX],
                BitmapFormat::Rgb => [src[0], src[1], src[2], u8::MAX],
                BitmapFormat::Rgba => [src[0], src[1], src[2], src[3]],
            })
        });
        let format = match compression {
            BitmapCompression::Bc1Rgb | BitmapCompression::Bc1Srgb => texpresso::Format::Bc1,
            BitmapCompression::Bc3 => texpresso::Format::Bc3,
            BitmapCompression::Bc4 => texpresso::Format::Bc4,
            BitmapCompression::Bc5 => texpresso::Format::Bc5,
        };
        let mut mips = Vec::with_capacity(bitmap.mip_levels() as usize);
        let params = texpresso::Params {
            algorithm,
            ..Default::default()
        };

        loop {
            let (width, height) = image.dimensions();
            let mut bytes = vec![0; format.compressed_size(width as usize, height as usize)];
            format.compress(
                image.as_raw(),
                width as usize,
                height as usize,
                params,
                &mut bytes,
            );
            mips.push(CompressedMip::new(width, height, bytes));

            if width == 1 && height == 1 {
                break;
            }
            let next_width = (width / 2).max(1);
            let next_height = (height / 2).max(1);
            image = if bitmap.color() == BitmapColor::Srgb {
                Self::resize_srgb(&image, next_width, next_height)
            } else {
                resize(&image, next_width, next_height, FilterType::CatmullRom)
            };
        }

        let compressed = CompressedBitmap::new(compression, mips);
        Self::write_texture_cache(&cache_key, &compressed);
        compressed
    }

    fn resize_srgb(image: &RgbaImage, width: u32, height: u32) -> RgbaImage {
        static SRGB_TO_LINEAR: OnceLock<[f32; 256]> = OnceLock::new();
        static LINEAR_TO_SRGB: OnceLock<Box<[u8; 65_536]>> = OnceLock::new();
        let srgb_to_linear = SRGB_TO_LINEAR.get_or_init(|| {
            std::array::from_fn(|value| {
                let value = value as f32 / u8::MAX as f32;
                if value <= 0.04045 {
                    value / 12.92
                } else {
                    ((value + 0.055) / 1.055).powf(2.4)
                }
            })
        });
        let linear_to_srgb = LINEAR_TO_SRGB.get_or_init(|| {
            Box::new(std::array::from_fn(|value| {
                let value = value as f32 / u16::MAX as f32;
                let value = if value <= 0.003_130_8 {
                    value * 12.92
                } else {
                    1.055 * value.powf(1.0 / 2.4) - 0.055
                };
                (value.clamp(0.0, 1.0) * u8::MAX as f32).round() as u8
            }))
        });
        let mut output = vec![0; width as usize * height as usize * 4];
        output.par_chunks_mut(4).enumerate().for_each(|(idx, dst)| {
            let x = idx as u32 % width;
            let y = idx as u32 / width;
            let x_start = x * image.width() / width;
            let x_end = ((x + 1) * image.width()).div_ceil(width).min(image.width());
            let y_start = y * image.height() / height;
            let y_end = ((y + 1) * image.height())
                .div_ceil(height)
                .min(image.height());
            let mut rgb = [0.0; 3];
            let mut alpha = 0.0;
            let mut count = 0;

            for src_y in y_start..y_end {
                for src_x in x_start..x_end {
                    let pixel = image.get_pixel(src_x, src_y).0;
                    for channel in 0..3 {
                        rgb[channel] += srgb_to_linear[pixel[channel] as usize];
                    }
                    alpha += pixel[3] as f32 / u8::MAX as f32;
                    count += 1;
                }
            }

            let scale = 1.0 / count as f32;
            for channel in 0..3 {
                let linear = (rgb[channel] * scale).clamp(0.0, 1.0);
                dst[channel] = linear_to_srgb[(linear * u16::MAX as f32).round() as usize];
            }
            dst[3] = ((alpha * scale).clamp(0.0, 1.0) * u8::MAX as f32).round() as u8;
        });
        RgbaImage::from_raw(width, height, output).unwrap()
    }

    fn compression_algorithm() -> texpresso::Algorithm {
        Self::compression_algorithm_for_profile(std::env::var("PROFILE").ok().as_deref())
    }

    fn compression_algorithm_for_profile(profile: Option<&str>) -> texpresso::Algorithm {
        if profile == Some("release") {
            texpresso::Algorithm::ClusterFit
        } else {
            texpresso::Algorithm::RangeFit
        }
    }

    fn texture_cache_key(
        bitmap: &Bitmap,
        compression: BitmapCompression,
        algorithm: texpresso::Algorithm,
    ) -> String {
        let mut hasher = blake3::Hasher::new();
        hasher.update(TEXTURE_CACHE_RECIPE);
        hasher.update(&bitmap.width().to_le_bytes());
        hasher.update(&bitmap.height().to_le_bytes());
        hasher.update(&bitmap.mip_levels().to_le_bytes());
        hasher.update(&[match bitmap.color() {
            BitmapColor::Linear => 0,
            BitmapColor::Srgb => 1,
        }]);
        hasher.update(&[match bitmap.format() {
            BitmapFormat::R => 0,
            BitmapFormat::Rg => 1,
            BitmapFormat::Rgb => 2,
            BitmapFormat::Rgba => 3,
        }]);
        hasher.update(&[match compression {
            BitmapCompression::Bc1Rgb => 0,
            BitmapCompression::Bc1Srgb => 1,
            BitmapCompression::Bc3 => 2,
            BitmapCompression::Bc4 => 3,
            BitmapCompression::Bc5 => 4,
        }]);
        hasher.update(&[if algorithm == texpresso::Algorithm::RangeFit {
            0
        } else {
            1
        }]);
        hasher.update(bitmap.pixels());
        hasher.finalize().to_hex().to_string()
    }

    fn texture_cache_dir() -> Option<&'static PathBuf> {
        static CACHE_DIR: OnceLock<Option<PathBuf>> = OnceLock::new();
        CACHE_DIR
            .get_or_init(|| {
                if std::env::var("PAK_TEXTURE_CACHE").as_deref() == Ok("off") {
                    return None;
                }
                if let Some(path) = std::env::var_os("PAK_TEXTURE_CACHE_DIR") {
                    return Some(PathBuf::from(path).join("v1"));
                }

                let out_dir = PathBuf::from(std::env::var_os("OUT_DIR")?);
                let build_dir = out_dir
                    .ancestors()
                    .find(|path| path.file_name().is_some_and(|name| name == "build"))?;
                Some(build_dir.parent()?.parent()?.join("pak-texture-cache/v1"))
            })
            .as_ref()
    }

    fn texture_cache_path(key: &str) -> Option<PathBuf> {
        Some(
            Self::texture_cache_dir()?
                .join(&key[..2])
                .join(format!("{key}.bin")),
        )
    }

    fn read_texture_cache(
        bitmap: &Bitmap,
        compression: BitmapCompression,
        key: &str,
    ) -> Option<CompressedBitmap> {
        let path = Self::texture_cache_path(key)?;
        let bytes = read(&path).ok()?;
        let Ok((compressed, consumed)) = bincode::serde::decode_from_slice::<CompressedBitmap, _>(
            &bytes,
            bincode::config::legacy(),
        ) else {
            let _ = remove_file(path);
            return None;
        };
        if consumed != bytes.len()
            || compressed.format() != compression
            || compressed
                .validate(bitmap.width(), bitmap.height(), bitmap.mip_levels())
                .is_err()
        {
            let _ = remove_file(path);
            return None;
        }
        Some(compressed)
    }

    fn write_texture_cache(key: &str, compressed: &CompressedBitmap) {
        let Some(path) = Self::texture_cache_path(key) else {
            return;
        };
        if path.exists() {
            return;
        }
        let Some(parent) = path.parent() else {
            return;
        };
        if let Err(error) = create_dir_all(parent) {
            warn!("unable to create BC texture cache directory: {error}");
            return;
        }
        let Ok(bytes) = bincode::serde::encode_to_vec(compressed, bincode::config::legacy()) else {
            return;
        };
        let temporary = parent.join(format!(
            ".{key}-{}-{}.tmp",
            std::process::id(),
            TEXTURE_CACHE_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        ));
        if let Err(error) = write(&temporary, bytes).and_then(|_| rename(&temporary, &path)) {
            let _ = remove_file(temporary);
            if !path.exists() {
                warn!("unable to publish BC texture cache entry: {error}");
            }
        }
    }

    /// Reads raw pixel data from an image source file and returns them in the given format.
    pub fn read_pixels(
        path: impl AsRef<Path>,
        swizzle: Option<BitmapSwizzle>,
        resize: Option<u32>,
    ) -> anyhow::Result<(BitmapFormat, u32, Vec<u8>)> {
        re_run_if_changed(&path);

        //let started = std::time::Instant::now();

        /*
            If this section ends up being very slow, it is usually because image was built in debug
            mode. You can use this for a regular build:

            [profile.dev.package.image]
            opt-level = 3

            But for a build.rs script you will need something a bit more invasive:

            [profile.dev.build-override]
            opt-level = 3 # Makes image 10x faster
            codegen-units = 1 # Makes image 2x faster (stacks with the above!)

            Obviously this will trade build time for runtime performance. PR this if you have better
            methods of handling this!!
        */

        let mut image = open(&path)
            .with_context(|| format!("Unable to open image file: {}", path.as_ref().display()))?;

        //let elapsed = std::time::Instant::now() - started;
        //info!("Image open took {} ms for {}x{}", elapsed.as_millis(), image.width(), image.height());

        // If format was not specified we guess (it is read as it is from disk; this
        // is just format represented in the .pak file and what you can retrieve it as)
        let swizzle = swizzle.unwrap_or_else(|| {
            match &image {
                DynamicImage::ImageLuma8(_) => BitmapSwizzle::One(BitmapChannel::R),
                DynamicImage::ImageRgb8(_) => {
                    BitmapSwizzle::Three([BitmapChannel::R, BitmapChannel::G, BitmapChannel::B])
                }
                DynamicImage::ImageRgba8(img) => {
                    if img.pixels().all(|pixel| pixel[3] == u8::MAX) {
                        // The source image has alpha but we're going to discard it
                        BitmapSwizzle::Three([BitmapChannel::R, BitmapChannel::G, BitmapChannel::B])
                    } else {
                        BitmapSwizzle::Four([
                            BitmapChannel::R,
                            BitmapChannel::G,
                            BitmapChannel::B,
                            BitmapChannel::A,
                        ])
                    }
                }
                _ => BitmapSwizzle::Four([
                    BitmapChannel::R,
                    BitmapChannel::G,
                    BitmapChannel::B,
                    BitmapChannel::A,
                ]),
            }
        });

        if let Some(resize) = resize {
            let (width, height) = if image.width() > image.height() {
                (resize, resize * image.height() / image.width())
            } else {
                (resize * image.width() / image.height(), resize)
            };
            let filter_ty = if image.width() == 1 && image.height() == 1 {
                FilterType::Nearest
            } else {
                FilterType::CatmullRom
            };

            image = image.resize_to_fill(width, height, filter_ty);
        }

        let image = match image {
            DynamicImage::ImageLuma8(image) => image.convert(),
            DynamicImage::ImageLumaA8(image) => image.convert(),
            DynamicImage::ImageRgb8(image) => image.convert(),
            DynamicImage::ImageRgba8(image) => image,
            DynamicImage::ImageLuma16(image) => image.convert(),
            DynamicImage::ImageLumaA16(image) => image.convert(),
            DynamicImage::ImageRgb16(image) => image.convert(),
            DynamicImage::ImageRgba16(image) => image.convert(),
            DynamicImage::ImageRgb32F(image) => image.convert(),
            DynamicImage::ImageRgba32F(image) => image.convert(),
            _ => unimplemented!(),
        };
        let width = image.width();
        let (format, data) = match swizzle {
            BitmapSwizzle::One(swizzle) => (BitmapFormat::R, Self::pixels_r(&image, swizzle)),
            BitmapSwizzle::Two(swizzle) => (BitmapFormat::Rg, Self::pixels_rg(&image, swizzle)),
            BitmapSwizzle::Three(swizzle) => (BitmapFormat::Rgb, Self::pixels_rgb(&image, swizzle)),
            BitmapSwizzle::Four(swizzle) => {
                (BitmapFormat::Rgba, Self::pixels_rgba(&image, swizzle))
            }
        };

        Ok((format, width, data))
    }

    fn pixels_r(image: &RgbaImage, r: BitmapChannel) -> Vec<u8> {
        let mut buf = Vec::with_capacity(image.width() as usize * image.height() as usize);
        for y in 0..image.height() {
            for x in 0..image.width() {
                let pixel = image.get_pixel(x, y);
                buf.push(pixel[r.rgba_index()]);
            }
        }

        buf
    }

    fn pixels_rg(image: &RgbaImage, [r, g]: [BitmapChannel; 2]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(image.width() as usize * image.height() as usize * 2);
        for y in 0..image.height() {
            for x in 0..image.width() {
                let pixel = image.get_pixel(x, y);
                buf.push(pixel[r.rgba_index()]);
                buf.push(pixel[g.rgba_index()]);
            }
        }

        buf
    }

    fn pixels_rgb(image: &RgbaImage, [r, g, b]: [BitmapChannel; 3]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(image.width() as usize * image.height() as usize * 3);
        for y in 0..image.height() {
            for x in 0..image.width() {
                let pixel = image.get_pixel(x, y);
                buf.push(pixel[r.rgba_index()]);
                buf.push(pixel[g.rgba_index()]);
                buf.push(pixel[b.rgba_index()]);
            }
        }

        buf
    }

    fn pixels_rgba(image: &RgbaImage, [r, g, b, a]: [BitmapChannel; 4]) -> Vec<u8> {
        let mut buf = Vec::with_capacity(image.width() as usize * image.height() as usize * 4);
        for y in 0..image.height() {
            for x in 0..image.width() {
                let pixel = image.get_pixel(x, y);
                buf.push(pixel[r.rgba_index()]);
                buf.push(pixel[g.rgba_index()]);
                buf.push(pixel[b.rgba_index()]);
                buf.push(pixel[a.rgba_index()]);
            }
        }

        buf
    }

    /// Sets the mesh file source.
    pub fn set_src(&mut self, src: impl AsRef<Path>) {
        self.src = Some(src.as_ref().to_path_buf());
    }

    /// The image file source.
    pub fn src(&self) -> Option<&Path> {
        self.src.as_deref()
    }
}

impl Canonicalize for BitmapAsset {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        if let Some(src) = self.src() {
            self.src = Some(Self::canonicalize_project_path(project_dir, src_dir, src));
        }
    }
}

/// Describes a single channel of a `Bitmap`.
#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, PartialEq)]
pub enum BitmapChannel {
    R,
    G,
    B,
    A,
}

impl BitmapChannel {
    fn rgba_index(self) -> usize {
        match self {
            Self::R => 0,
            Self::G => 1,
            Self::B => 2,
            Self::A => 3,
        }
    }
}

/// Describes the channel arrangement of a `Bitmap`.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum BitmapSwizzle {
    One(BitmapChannel),
    Two([BitmapChannel; 2]),
    Three([BitmapChannel; 3]),
    Four([BitmapChannel; 4]),
}

impl BitmapSwizzle {
    pub const RGB: Self = Self::Three([BitmapChannel::R, BitmapChannel::G, BitmapChannel::B]);
    pub const RGBA: Self = Self::Four([
        BitmapChannel::R,
        BitmapChannel::G,
        BitmapChannel::B,
        BitmapChannel::A,
    ]);

    fn de<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ScalarRefVisitor;

        impl Visitor<'_> for ScalarRefVisitor {
            type Value = Option<BitmapSwizzle>;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("swizzle string with one to four values of either r, g, b or a")
            }

            fn visit_str<E>(self, str: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                fn parse_channel<E>(c: char) -> Result<BitmapChannel, E>
                where
                    E: serde::de::Error,
                {
                    Ok(match c {
                        'r' => BitmapChannel::R,
                        'g' => BitmapChannel::G,
                        'b' => BitmapChannel::B,
                        'a' => BitmapChannel::A,
                        _ => return Err(E::custom("expected a value of either r, g, b or a")),
                    })
                }

                let mut chars = str.chars();

                Ok(Some(match str.len() {
                    1 => BitmapSwizzle::One(parse_channel(
                        chars
                            .next()
                            .ok_or_else(|| E::custom("unexpected end of swizzle string"))?,
                    )?),
                    2 => BitmapSwizzle::Two([
                        parse_channel(
                            chars
                                .next()
                                .ok_or_else(|| E::custom("unexpected end of swizzle string"))?,
                        )?,
                        parse_channel(
                            chars
                                .next()
                                .ok_or_else(|| E::custom("unexpected end of swizzle string"))?,
                        )?,
                    ]),
                    3 => BitmapSwizzle::Three([
                        parse_channel(
                            chars
                                .next()
                                .ok_or_else(|| E::custom("unexpected end of swizzle string"))?,
                        )?,
                        parse_channel(
                            chars
                                .next()
                                .ok_or_else(|| E::custom("unexpected end of swizzle string"))?,
                        )?,
                        parse_channel(
                            chars
                                .next()
                                .ok_or_else(|| E::custom("unexpected end of swizzle string"))?,
                        )?,
                    ]),
                    4 => BitmapSwizzle::Four([
                        parse_channel(
                            chars
                                .next()
                                .ok_or_else(|| E::custom("unexpected end of swizzle string"))?,
                        )?,
                        parse_channel(
                            chars
                                .next()
                                .ok_or_else(|| E::custom("unexpected end of swizzle string"))?,
                        )?,
                        parse_channel(
                            chars
                                .next()
                                .ok_or_else(|| E::custom("unexpected end of swizzle string"))?,
                        )?,
                        parse_channel(
                            chars
                                .next()
                                .ok_or_else(|| E::custom("unexpected end of swizzle string"))?,
                        )?,
                    ]),
                    _ => return Err(E::custom("expected a string with one to four values")),
                }))
            }
        }

        deserializer.deserialize_any(ScalarRefVisitor)
    }
}

#[cfg(test)]
mod test {
    use {super::*, toml::de::ValueDeserializer};

    #[test]
    fn compression_quality_follows_profile() {
        assert!(
            BitmapAsset::compression_algorithm_for_profile(Some("debug"))
                == texpresso::Algorithm::RangeFit
        );
        assert!(
            BitmapAsset::compression_algorithm_for_profile(Some("release"))
                == texpresso::Algorithm::ClusterFit
        );
    }

    #[test]
    fn srgb_resize_averages_in_linear_space() {
        let image = RgbaImage::from_raw(
            2,
            1,
            vec![0, 0, 0, u8::MAX, u8::MAX, u8::MAX, u8::MAX, u8::MAX],
        )
        .unwrap();
        let resized = BitmapAsset::resize_srgb(&image, 1, 1);

        assert_eq!(resized.get_pixel(0, 0).0, [188, 188, 188, u8::MAX]);
    }

    #[test]
    fn texture_cache_key_includes_pixels_and_quality() {
        let a = Bitmap::new(BitmapColor::Srgb, BitmapFormat::Rgba, 1, 1, [1, 2, 3, 4]);
        let b = Bitmap::new(BitmapColor::Srgb, BitmapFormat::Rgba, 1, 1, [4, 3, 2, 1]);
        let a_fast = BitmapAsset::texture_cache_key(
            &a,
            BitmapCompression::Bc3,
            texpresso::Algorithm::RangeFit,
        );
        let a_high = BitmapAsset::texture_cache_key(
            &a,
            BitmapCompression::Bc3,
            texpresso::Algorithm::ClusterFit,
        );
        let b_fast = BitmapAsset::texture_cache_key(
            &b,
            BitmapCompression::Bc3,
            texpresso::Algorithm::RangeFit,
        );

        assert_ne!(a_fast, a_high);
        assert_ne!(a_fast, b_fast);
    }

    #[test]
    fn compression_setting() {
        for (name, expected) in [
            ("bc1-rgb", BitmapCompression::Bc1Rgb),
            ("bc1-srgb", BitmapCompression::Bc1Srgb),
            ("bc3", BitmapCompression::Bc3),
            ("bc4", BitmapCompression::Bc4),
            ("bc5", BitmapCompression::Bc5),
        ] {
            let asset = BitmapAsset::deserialize(parse_toml(&format!(
                "{{ src = '', compression = '{name}' }}"
            )))
            .expect("compression setting should deserialize");
            assert_eq!(asset.compression(), Some(expected));
        }
    }

    #[test]
    fn default_texture_compression_preserves_explicit_bitmap_compression() {
        let asset = BitmapAsset::deserialize(parse_toml(
            "{ src = '', color = 'linear', compression = 'bc5' }",
        ))
        .unwrap()
        .with_default_color_compression();

        assert_eq!(asset.compression(), Some(BitmapCompression::Bc5));
    }

    #[test]
    fn compressed_mips_include_block_padded_npot_levels() {
        let bitmap = Bitmap::new(
            BitmapColor::Srgb,
            BitmapFormat::Rgba,
            5,
            3,
            vec![127; 5 * 3 * 4],
        );

        for (format, expected_lengths) in [
            (BitmapCompression::Bc1Rgb, vec![16, 8, 8]),
            (BitmapCompression::Bc1Srgb, vec![16, 8, 8]),
            (BitmapCompression::Bc3, vec![32, 16, 16]),
            (BitmapCompression::Bc4, vec![16, 8, 8]),
            (BitmapCompression::Bc5, vec![32, 16, 16]),
        ] {
            let compressed = BitmapAsset::compress(&bitmap, format);
            assert_eq!(compressed.format(), format);
            assert_eq!(
                compressed
                    .mips()
                    .iter()
                    .map(CompressedMip::extent)
                    .collect::<Vec<_>>(),
                [(5, 3), (2, 1), (1, 1)]
            );
            assert_eq!(
                compressed
                    .mips()
                    .iter()
                    .map(|mip| mip.bytes().len())
                    .collect::<Vec<_>>(),
                expected_lengths
            );
        }
    }

    #[test]
    fn mip_levels() {
        assert!(BitmapAsset::deserialize(parse_toml("{ src = '', mip-levels = '' }")).is_err());
        assert!(BitmapAsset::deserialize(parse_toml("{ src = '', mip-levels = 42.0 }")).is_err());
        assert!(BitmapAsset::deserialize(parse_toml("{ src = '', mip-levels = -42 }")).is_err());
        assert!(BitmapAsset::deserialize(parse_toml("{ src = '', mip-levels = [] }")).is_err());
        assert!(BitmapAsset::deserialize(parse_toml("{ src = '', mip-levels = {} }")).is_err());

        assert!(BitmapAsset::deserialize(parse_toml("{ src = '', mip-levels = 0 }")).is_err(),);

        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '' }"))
                .expect("deserialize test config should succeed"),
            BitmapAsset::new(PathBuf::new()).with_mip_levels(MIP_LEVELS_MIN),
        );

        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', mip-levels = 100 }"))
                .expect("deserialize with mip-levels=100 should succeed"),
            BitmapAsset::new(PathBuf::new()).with_mip_levels(MIP_LEVELS_MAX),
        );
        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', mip-levels = false }"))
                .expect("deserialize with mip-levels=false should succeed"),
            BitmapAsset::new(PathBuf::new()).with_mip_levels(MIP_LEVELS_MIN),
        );
        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', mip-levels = true }"))
                .expect("deserialize with mip-levels=true should succeed"),
            BitmapAsset::new(PathBuf::new()).with_mip_levels(MIP_LEVELS_MAX),
        );
        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', mip-levels = 16 }"))
                .expect("deserialize with mip-levels=16 should succeed"),
            BitmapAsset::new(PathBuf::new()).with_mip_levels(16),
        );
    }

    #[test]
    fn swizzle() {
        assert!(BitmapAsset::deserialize(parse_toml("{ src = '', swizzle = '' }")).is_err(),);
        assert!(BitmapAsset::deserialize(parse_toml("{ src = '', swizzle = 'rrggbb' }")).is_err(),);
        assert!(BitmapAsset::deserialize(parse_toml("{ src = '', swizzle = 'z' }")).is_err(),);

        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', swizzle = 'r' }"))
                .expect("deserialize with swizzle='r' should succeed"),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::One(BitmapChannel::R))
        );
        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', swizzle = 'g' }"))
                .expect("deserialize with swizzle='g' should succeed"),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::One(BitmapChannel::G))
        );
        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', swizzle = 'b' }"))
                .expect("deserialize with swizzle='b' should succeed"),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::One(BitmapChannel::B))
        );
        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', swizzle = 'a' }"))
                .expect("deserialize with swizzle='a' should succeed"),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::One(BitmapChannel::A))
        );

        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', swizzle = 'gg' }"))
                .expect("deserialize with swizzle='gg' should succeed"),
            BitmapAsset::new(PathBuf::new())
                .with_swizzle(BitmapSwizzle::Two([BitmapChannel::G, BitmapChannel::G]))
        );
        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', swizzle = 'bgr' }"))
                .expect("deserialize with swizzle='bgr' should succeed"),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::Three([
                BitmapChannel::B,
                BitmapChannel::G,
                BitmapChannel::R
            ]))
        );
        assert_eq!(
            BitmapAsset::deserialize(parse_toml("{ src = '', swizzle = 'rrrr' }"))
                .expect("deserialize with swizzle='rrrr' should succeed"),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::Four([
                BitmapChannel::R,
                BitmapChannel::R,
                BitmapChannel::R,
                BitmapChannel::R
            ]))
        );
    }

    fn parse_toml<'a>(raw: &'a str) -> ValueDeserializer<'a> {
        ValueDeserializer::parse(raw).expect("valid toml should parse")
    }
}
