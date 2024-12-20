use {
    super::{file_key, re_run_if_changed, Canonicalize, Writer},
    crate::{
        bitmap::{Bitmap, BitmapColor, BitmapFormat},
        BitmapId,
    },
    anyhow::Context,
    image::{buffer::ConvertBuffer, imageops::FilterType, open, DynamicImage, RgbaImage},
    log::info,
    parking_lot::Mutex,
    serde::{de::Visitor, Deserialize, Deserializer},
    std::{
        fmt::Formatter,
        path::{Path, PathBuf},
        sync::Arc,
    },
};

/// Holds a description of `.jpeg` and other regular images.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct BitmapAsset {
    color: Option<BitmapColor>,
    resize: Option<u32>,
    src: PathBuf,

    /// Controls the number and order of the channels output into the final image
    #[serde(default, deserialize_with = "BitmapSwizzle::de")]
    swizzle: Option<BitmapSwizzle>,
}

impl BitmapAsset {
    /// Constructs a new Bitmap with the given image file source.
    pub fn new<P>(src: P) -> Self
    where
        P: AsRef<Path>,
    {
        Self {
            color: None,
            resize: None,
            src: src.as_ref().to_path_buf(),
            swizzle: None,
        }
    }

    #[allow(dead_code)]
    pub fn with_color(mut self, color: BitmapColor) -> Self {
        self.color = Some(color);
        self
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
        // Early-out if we have already baked this bitmap
        let asset = self.clone().into();
        if let Some(id) = writer.lock().ctx.get(&asset) {
            return Ok(id.as_bitmap().unwrap());
        }

        let key = path.as_ref().map(|path| file_key(&project_dir, path));
        if let Some(key) = &key {
            // This bitmap will be accessible using this key
            info!("Baking bitmap: {}", key);
        } else {
            // This bitmap will only be accessible using the id
            info!(
                "Baking bitmap: {} (inline)",
                file_key(&project_dir, self.src())
            );
        }

        let bitmap = self
            .as_bitmap_buf()
            .context("Unable to create bitmap buf")?;

        let mut writer = writer.lock();
        if let Some(id) = writer.ctx.get(&asset) {
            return Ok(id.as_bitmap().unwrap());
        }

        let id = writer.push_bitmap(bitmap, key);
        writer.ctx.insert(asset, id.into());

        Ok(id)
    }

    pub fn as_bitmap_buf(&self) -> anyhow::Result<Bitmap> {
        let (format, width, pixels) = Self::read_pixels(self.src(), self.swizzle, self.resize)
            .context("Unable to read pixels")?;

        Ok(Bitmap::new(self.color(), format, width, pixels))
    }

    pub fn color(&self) -> BitmapColor {
        self.color.unwrap_or(BitmapColor::Srgb)
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

    /// The image file source.
    pub fn src(&self) -> &Path {
        self.src.as_path()
    }
}

impl Canonicalize for BitmapAsset {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        self.src = Self::canonicalize_project_path(project_dir, src_dir, &self.src);
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
        BitmapChannel::B,
        BitmapChannel::G,
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
                    1 => BitmapSwizzle::One(parse_channel(chars.next().unwrap())?),
                    2 => BitmapSwizzle::Two([
                        parse_channel(chars.next().unwrap())?,
                        parse_channel(chars.next().unwrap())?,
                    ]),
                    3 => BitmapSwizzle::Three([
                        parse_channel(chars.next().unwrap())?,
                        parse_channel(chars.next().unwrap())?,
                        parse_channel(chars.next().unwrap())?,
                    ]),
                    4 => BitmapSwizzle::Four([
                        parse_channel(chars.next().unwrap())?,
                        parse_channel(chars.next().unwrap())?,
                        parse_channel(chars.next().unwrap())?,
                        parse_channel(chars.next().unwrap())?,
                    ]),
                    _ => return Err(E::custom("expected a string with one to four values")),
                }))
            }
        }

        deserializer.deserialize_any(ScalarRefVisitor)
    }
}

#[cfg(test)]
mod tests {
    use {super::*, toml::de::ValueDeserializer};

    #[test]
    fn bitmap_swizzle() {
        assert!(
            BitmapAsset::deserialize(ValueDeserializer::new("{ src = '', swizzle = '' }")).is_err(),
        );
        assert!(BitmapAsset::deserialize(ValueDeserializer::new(
            "{ src = '', swizzle = 'rrggbb' }"
        ))
        .is_err(),);
        assert!(
            BitmapAsset::deserialize(ValueDeserializer::new("{ src = '', swizzle = 'z' }"))
                .is_err(),
        );

        assert_eq!(
            BitmapAsset::deserialize(ValueDeserializer::new("{ src = '', swizzle = 'r' }"))
                .unwrap(),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::One(BitmapChannel::R))
        );
        assert_eq!(
            BitmapAsset::deserialize(ValueDeserializer::new("{ src = '', swizzle = 'g' }"))
                .unwrap(),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::One(BitmapChannel::G))
        );
        assert_eq!(
            BitmapAsset::deserialize(ValueDeserializer::new("{ src = '', swizzle = 'b' }"))
                .unwrap(),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::One(BitmapChannel::B))
        );
        assert_eq!(
            BitmapAsset::deserialize(ValueDeserializer::new("{ src = '', swizzle = 'a' }"))
                .unwrap(),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::One(BitmapChannel::A))
        );

        assert_eq!(
            BitmapAsset::deserialize(ValueDeserializer::new("{ src = '', swizzle = 'gg' }"))
                .unwrap(),
            BitmapAsset::new(PathBuf::new())
                .with_swizzle(BitmapSwizzle::Two([BitmapChannel::G, BitmapChannel::G]))
        );
        assert_eq!(
            BitmapAsset::deserialize(ValueDeserializer::new("{ src = '', swizzle = 'bgr' }"))
                .unwrap(),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::Three([
                BitmapChannel::B,
                BitmapChannel::G,
                BitmapChannel::R
            ]))
        );
        assert_eq!(
            BitmapAsset::deserialize(ValueDeserializer::new("{ src = '', swizzle = 'rrrr' }"))
                .unwrap(),
            BitmapAsset::new(PathBuf::new()).with_swizzle(BitmapSwizzle::Four([
                BitmapChannel::R,
                BitmapChannel::R,
                BitmapChannel::R,
                BitmapChannel::R
            ]))
        );
    }
}
