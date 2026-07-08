use {
    super::{
        Asset, Canonicalize, Writer,
        bitmap::{BitmapAsset, BitmapSwizzle},
        file_key, is_toml, parse_hex_color, parse_hex_scalar,
    },
    crate::{
        BitmapId, MaterialId, MaterialInfo, MaterialParameterFlags,
        bitmap::{Bitmap, BitmapColor, BitmapFormat},
    },
    anyhow::Context as _,
    image::{DynamicImage, GenericImageView, GrayImage, imageops::FilterType},
    log::{info, warn},
    ordered_float::OrderedFloat,
    parking_lot::Mutex,
    serde::{
        Deserialize, Deserializer,
        de::{
            Error, MapAccess, SeqAccess, Visitor,
            value::{MapAccessDeserializer, SeqAccessDeserializer},
        },
    },
    std::{
        fmt::Formatter,
        num::FpCategory,
        path::{Path, PathBuf},
        sync::Arc,
    },
    tokio::runtime::Runtime,
};

/// A reference to a `Bitmap` asset, `Bitmap` asset file, three or four channel image source file,
/// or single four channel color.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ColorRef {
    /// A `Bitmap` asset specified inline.
    Asset(BitmapAsset),

    /// A `Bitmap` asset file or image source file.
    Path(PathBuf),

    /// A single four channel color.
    Value([OrderedFloat<f32>; 4]),
}

impl ColorRef {
    pub const WHITE: Self = Self::Value([OrderedFloat(1.0f32); 4]);

    /// Deserialize from any of:
    ///
    /// val of [0.666, 0.733, 0.8, 1.0]:
    /// .. = "#abc"
    /// .. = "#abcf"
    /// .. = "#aabbcc"
    /// .. = "#aabbccff"
    /// .. = [0.666, 0.733, 0.8, 1.0]
    ///
    /// src of file.png:
    /// .. = "file.png"
    ///
    /// src of file.toml which must be a `Bitmap` asset:
    /// .. = "file.toml"
    ///
    /// src of a `Bitmap` asset:
    /// .. = { src = "file.png", format = "rgb" }
    fn de<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ColorRefVisitor;

        impl<'de> Visitor<'de> for ColorRefVisitor {
            type Value = Option<ColorRef>;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("hex string, path string, bitmap asset, or sequence")
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let asset = Deserialize::deserialize(MapAccessDeserializer::new(map))?;

                Ok(Some(ColorRef::Asset(asset)))
            }

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let mut val: Vec<f32> = Deserialize::deserialize(SeqAccessDeserializer::new(seq))?;
                for val in &val {
                    match val.classify() {
                        FpCategory::Zero | FpCategory::Normal if (0.0..=1.0).contains(val) => (),
                        _ => {
                            return Err(Error::custom(
                                "expected a color value between 0.0 and 1.0",
                            ));
                        }
                    }
                }

                match val.len() {
                    3 => val.push(1.0),
                    4 => (),
                    _ => return Err(Error::custom("expected 3 or 4 color channels")),
                }

                Ok(Some(ColorRef::Value([
                    OrderedFloat(val[0]),
                    OrderedFloat(val[1]),
                    OrderedFloat(val[2]),
                    OrderedFloat(val[3]),
                ])))
            }

            fn visit_str<E>(self, str: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if str.starts_with('#')
                    && let Some(val) = parse_hex_color(str)
                {
                    return Ok(Some(ColorRef::Value([
                        OrderedFloat(val[0] as f32 / u8::MAX as f32),
                        OrderedFloat(val[1] as f32 / u8::MAX as f32),
                        OrderedFloat(val[2] as f32 / u8::MAX as f32),
                        OrderedFloat(val[3] as f32 / u8::MAX as f32),
                    ])));
                }

                Ok(Some(ColorRef::Path(PathBuf::from(str))))
            }
        }

        deserializer.deserialize_any(ColorRefVisitor)
    }
}

impl Canonicalize for ColorRef {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        match self {
            Self::Asset(bitmap) => bitmap.canonicalize(project_dir, src_dir),
            Self::Path(src) => *src = Self::canonicalize_project_path(project_dir, src_dir, &src),
            _ => (),
        }
    }
}

impl Default for ColorRef {
    fn default() -> Self {
        Self::WHITE
    }
}

/// A reference to a `Bitmap` asset, `Bitmap` asset file, three channel image source file,
/// or single three channel color.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum EmissiveRef {
    /// A `Bitmap` asset specified inline.
    Asset(BitmapAsset),

    /// A `Bitmap` asset file or image source file.
    Path(PathBuf),

    /// A single three channel color.
    Value([OrderedFloat<f32>; 3]),
}

impl EmissiveRef {
    pub const WHITE: Self = Self::Value([OrderedFloat(1.0); 3]);

    /// Deserialize from any of:
    ///
    /// val of [0.666, 0.733, 0.8]:
    /// .. = "#abc"
    /// .. = "#aabbcc"
    /// .. = [0.666, 0.733, 0.8]
    ///
    /// src of file.png:
    /// .. = "file.png"
    ///
    /// src of file.toml which must be a `Bitmap` asset:
    /// .. = "file.toml"
    ///
    /// src of a `Bitmap` asset:
    /// .. = { src = "file.png", format = "rgb" }
    fn de<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct EmissiveRefVisitor;

        impl<'de> Visitor<'de> for EmissiveRefVisitor {
            type Value = Option<EmissiveRef>;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("hex string, path string, bitmap asset, or sequence")
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let asset = Deserialize::deserialize(MapAccessDeserializer::new(map))?;

                Ok(Some(EmissiveRef::Asset(asset)))
            }

            fn visit_seq<A>(self, seq: A) -> Result<Self::Value, A::Error>
            where
                A: SeqAccess<'de>,
            {
                let val: Vec<f32> = Deserialize::deserialize(SeqAccessDeserializer::new(seq))?;
                for val in &val {
                    match val.classify() {
                        FpCategory::Zero | FpCategory::Normal if (0.0..=1.0).contains(val) => (),
                        _ => {
                            return Err(Error::custom(
                                "expected a color value between 0.0 and 1.0",
                            ));
                        }
                    }
                }

                if val.len() != 3 {
                    return Err(Error::custom("expected 3 color channels"));
                }

                Ok(Some(EmissiveRef::Value([
                    OrderedFloat(val[0]),
                    OrderedFloat(val[1]),
                    OrderedFloat(val[2]),
                ])))
            }

            fn visit_str<E>(self, str: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if str.starts_with('#')
                    && let Some(val) = parse_hex_color(str)
                {
                    assert_eq!(val[3], u8::MAX);

                    return Ok(Some(EmissiveRef::Value([
                        OrderedFloat(val[0] as f32 / u8::MAX as f32),
                        OrderedFloat(val[1] as f32 / u8::MAX as f32),
                        OrderedFloat(val[2] as f32 / u8::MAX as f32),
                    ])));
                }

                Ok(Some(EmissiveRef::Path(PathBuf::from(str))))
            }
        }

        deserializer.deserialize_any(EmissiveRefVisitor)
    }
}

impl Canonicalize for EmissiveRef {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        match self {
            Self::Asset(bitmap) => bitmap.canonicalize(project_dir, src_dir),
            Self::Path(src) => *src = Self::canonicalize_project_path(project_dir, src_dir, &src),
            _ => (),
        }
    }
}

impl Default for EmissiveRef {
    fn default() -> Self {
        Self::WHITE
    }
}

/// Holds a description of data used for mesh rendering.
#[derive(Clone, Debug, Default, Deserialize, Eq, Hash, PartialEq)]
#[serde(default, rename_all = "kebab-case")]
pub struct MaterialAsset {
    /// A `Bitmap` asset, `Bitmap` asset file, three or four channel image source file, or single
    /// four channel color.
    #[serde(deserialize_with = "ColorRef::de")]
    pub color: Option<ColorRef>,

    #[serde(deserialize_with = "ScalarRef::de")]
    pub height: Option<ScalarRef>,

    /// Whether or not the mesh will be rendered with back faces also enabled.
    pub double_sided: Option<bool>,

    /// A `Bitmap` asset, `Bitmap` asset file, three channel image source file, or a single
    /// three channel color.
    #[serde(deserialize_with = "EmissiveRef::de")]
    pub emissive: Option<EmissiveRef>,

    /// A `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a single
    /// normalized value.
    #[serde(deserialize_with = "ScalarRef::de")]
    pub metal: Option<ScalarRef>,

    /// A bitmap asset, bitmap asset file, or a three channel image.
    #[serde(deserialize_with = "NormalRef::de")]
    pub normal: Option<NormalRef>,

    /// A `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a single
    /// normalized value.
    #[serde(deserialize_with = "ScalarRef::de")]
    pub rough: Option<ScalarRef>,

    /// A `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a single
    /// normalized value.
    #[serde(deserialize_with = "ScalarRef::de")]
    pub transmission: Option<ScalarRef>,
}

impl MaterialAsset {
    #[allow(unused)]
    pub(crate) fn new<P>(src: P) -> Self
    where
        P: AsRef<Path>,
    {
        Self {
            color: Some(ColorRef::Path(src.as_ref().to_owned())),
            ..Default::default()
        }
    }

    /// Reads and processes 3D mesh material source files into an existing `.pak` file buffer.
    pub(super) fn bake(
        &mut self,
        rt: &Runtime,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        src_dir: impl AsRef<Path>,
        path: Option<impl AsRef<Path>>,
    ) -> anyhow::Result<MaterialId> {
        // Early-out if we have already baked this material
        let asset = self.clone().into();
        if let Some(id) = writer.lock().ctx.get(&asset) {
            return id
                .as_material()
                .context("asset context returned non-material id");
        }

        // If a source is given it will be available as a key inside the .pak (sources are not
        // given if the asset is specified inline - those are only available in the .pak via ID)
        let key = path.as_ref().map(|path| file_key(&project_dir, path));
        if let Some(key) = &key {
            // This material will be accessible using this key
            info!("Baking material: {}", key);
        } else {
            // This material will only be accessible using the ID
            info!("Baking material: (inline)");
        }

        let material_info = self.as_material_info(rt, writer, project_dir, src_dir)?;

        let mut writer = writer.lock();
        if let Some(id) = writer.ctx.get(&asset) {
            return id
                .as_material()
                .context("asset context returned non-material id");
        }

        let id = writer.push_material(material_info, key);
        writer.ctx.insert(asset, id.into());

        Ok(id)
    }

    fn as_material_info(
        &mut self,
        rt: &Runtime,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        src_dir: impl AsRef<Path>,
    ) -> anyhow::Result<MaterialInfo> {
        let color = match &self.color {
            Some(ColorRef::Asset(bitmap)) => {
                let writer = writer.clone();
                let project_dir = project_dir.as_ref().to_path_buf();
                let mut bitmap = bitmap.clone();

                rt.spawn_blocking(move || {
                    bitmap
                        .bake(&writer, &project_dir)
                        .context("Unable to bake color asset bitmap")
                })
            }
            Some(ColorRef::Path(src)) => {
                let mut bitmap = if is_toml(src) {
                    let mut bitmap = Asset::read(src)
                        .context("Unable to read color bitmap asset")?
                        .into_bitmap()
                        .context("Source file should be a bitmap asset")?;
                    bitmap.canonicalize(&project_dir, &src_dir);
                    bitmap
                } else {
                    BitmapAsset::new(src)
                };
                let writer = writer.clone();
                let project_dir = project_dir.as_ref().to_path_buf();

                rt.spawn_blocking(move || {
                    bitmap
                        .bake_from_path(&writer, &project_dir, Option::<PathBuf>::None)
                        .context("Unable to bake color asset bitmap from path")
                })
            }
            &Some(ColorRef::Value(val)) => {
                let writer = writer.clone();

                rt.spawn_blocking(move || -> anyhow::Result<BitmapId> {
                    let mut writer = writer.lock();
                    if let Some(id) = writer.ctx.get(&Asset::ColorRgba(val)) {
                        id.as_bitmap().context("expected bitmap id for color value")
                    } else {
                        let bitmap = Bitmap::new(
                            BitmapColor::Linear,
                            BitmapFormat::Rgba,
                            1,
                            1,
                            [
                                (val[0].0 * u8::MAX as f32) as u8,
                                (val[1].0 * u8::MAX as f32) as u8,
                                (val[2].0 * u8::MAX as f32) as u8,
                                (val[3].0 * u8::MAX as f32) as u8,
                            ],
                        );
                        Ok(writer.push_bitmap(bitmap, None))
                    }
                })
            }
            None => {
                let writer = writer.clone();

                rt.spawn_blocking(move || -> anyhow::Result<BitmapId> {
                    let potters_clay =
                        parse_hex_color("#8C5738").expect("compile-time hex color is valid");
                    let potters_clay = [
                        OrderedFloat(potters_clay[0] as f32 / u8::MAX as f32),
                        OrderedFloat(potters_clay[1] as f32 / u8::MAX as f32),
                        OrderedFloat(potters_clay[2] as f32 / u8::MAX as f32),
                        OrderedFloat(1.0),
                    ];
                    let mut writer = writer.lock();
                    if let Some(id) = writer.ctx.get(&Asset::ColorRgba(potters_clay)) {
                        id.as_bitmap()
                            .context("expected bitmap id for default color")
                    } else {
                        let bitmap = Bitmap::new(
                            BitmapColor::Linear,
                            BitmapFormat::Rgb,
                            1,
                            1,
                            [
                                (potters_clay[0].0 * u8::MAX as f32) as u8,
                                (potters_clay[1].0 * u8::MAX as f32) as u8,
                                (potters_clay[2].0 * u8::MAX as f32) as u8,
                                (potters_clay[3].0 * u8::MAX as f32) as u8,
                            ],
                        );
                        Ok(writer.push_bitmap(bitmap, None))
                    }
                })
            }
        };

        let normal = self
            .normal
            .as_ref()
            .map(|normal| {
                anyhow::Ok(match normal {
                    NormalRef::Asset(bitmap) => {
                        let writer = writer.clone();
                        let project_dir = project_dir.as_ref().to_path_buf();
                        let mut bitmap = bitmap.clone().with_swizzle(BitmapSwizzle::RGB);

                        rt.spawn_blocking(move || {
                            Self::bake_normal_bitmap(
                                &mut bitmap,
                                &writer,
                                &project_dir,
                                None::<PathBuf>,
                            )
                            .context("Unable to bake normal asset bitmap")
                        })
                    }
                    NormalRef::Path(src) => {
                        let mut bitmap = if is_toml(src) {
                            let mut bitmap = Asset::read(src)
                                .context("Unable to read normal bitmap asset")?
                                .into_bitmap()
                                .context("Source file should be a bitmap asset")?;
                            bitmap.canonicalize(&project_dir, &src_dir);
                            bitmap
                        } else {
                            BitmapAsset::new(src)
                        };
                        let writer = writer.clone();
                        let project_dir = project_dir.as_ref().to_path_buf();

                        rt.spawn_blocking(move || {
                            bitmap = bitmap.with_swizzle(BitmapSwizzle::RGB);
                            Self::bake_normal_bitmap(
                                &mut bitmap,
                                &writer,
                                &project_dir,
                                None::<PathBuf>,
                            )
                            .context("Unable to bake normal asset bitmap from path")
                        })
                    }
                })
            })
            .transpose()?;

        let emissive = self
            .emissive
            .as_ref()
            .map(|emissive| {
                anyhow::Ok(match emissive {
                    EmissiveRef::Asset(bitmap) => {
                        let writer = writer.clone();
                        let project_dir = project_dir.as_ref().to_path_buf();
                        let mut bitmap = bitmap.clone().with_swizzle(BitmapSwizzle::RGB);

                        rt.spawn_blocking(move || -> anyhow::Result<BitmapId> {
                            bitmap
                                .bake(&writer, &project_dir)
                                .context("Unable to bake emissive asset bitmap")
                        })
                    }
                    EmissiveRef::Path(src) => {
                        let bitmap = if is_toml(src) {
                            let mut bitmap = Asset::read(src)
                                .context("Unable to read emissive bitmap asset")?
                                .into_bitmap()
                                .context("Source file should be a bitmap asset")?;
                            bitmap.canonicalize(&project_dir, &src_dir);
                            bitmap
                        } else {
                            BitmapAsset::new(src)
                        };
                        let writer = writer.clone();
                        let project_dir = project_dir.as_ref().to_path_buf();

                        rt.spawn_blocking(move || -> anyhow::Result<BitmapId> {
                            bitmap
                                .with_swizzle(BitmapSwizzle::RGB)
                                .bake_from_path(&writer, &project_dir, Option::<PathBuf>::None)
                                .context("Unable to bake emissive asset bitmap from path")
                        })
                    }
                    EmissiveRef::Value(val) => {
                        let writer = writer.clone();
                        let val = *val;

                        rt.spawn_blocking(move || -> anyhow::Result<BitmapId> {
                            let mut writer = writer.lock();
                            if let Some(id) = writer.ctx.get(&Asset::ColorRgb(val)) {
                                id.as_bitmap().context("expected bitmap id for emissive")
                            } else {
                                let bitmap = Bitmap::new(
                                    BitmapColor::Linear,
                                    BitmapFormat::Rgb,
                                    1,
                                    1,
                                    [
                                        (val[0].0 * u8::MAX as f32) as u8,
                                        (val[1].0 * u8::MAX as f32) as u8,
                                        (val[2].0 * u8::MAX as f32) as u8,
                                    ],
                                );
                                Ok(writer.push_bitmap(bitmap, None))
                            }
                        })
                    }
                })
            })
            .transpose()?;

        let mut params_used = MaterialParameterFlags::empty();
        if self.metal.is_some() {
            params_used |= MaterialParameterFlags::METAL;
        }
        if self.rough.is_some() {
            params_used |= MaterialParameterFlags::ROUGH;
        }
        if self.height.is_some() {
            params_used |= MaterialParameterFlags::HEIGHT;
        }
        if self.transmission.is_some() {
            params_used |= MaterialParameterFlags::TRANSMISSION;
        }

        let height_ref = self.height.clone();
        let metal = self.metal.clone();
        let rough = self.rough.clone();
        let transmission = self.transmission.clone();
        let params_asset = Asset::MaterialParams(MaterialParams {
            height: height_ref,
            metal,
            rough,
            transmission,
        });
        let use_params = !params_used.is_empty();
        let params = use_params.then(|| {
            let project_dir = project_dir.as_ref().to_path_buf();
            let src_dir = src_dir.as_ref().to_path_buf();
            let writer = writer.clone();
            let height_ref = self.height.clone();
            let metal = self.metal.clone();
            let rough = self.rough.clone();
            let transmission = self.transmission.clone();

            rt.spawn_blocking(move || {
                if let Some(id) = writer.lock().ctx.get(&params_asset) {
                    return id.as_bitmap().context("expected bitmap id for params");
                }

                let mut metal_image = DynamicImage::ImageLuma8(
                    Self::scalar_ref_into_gray_image(&metal, &project_dir, &src_dir, 0)
                        .context("Unable to create metal bitmap buf")?,
                );
                let mut rough_image = DynamicImage::ImageLuma8(
                    Self::scalar_ref_into_gray_image(&rough, &project_dir, &src_dir, u8::MAX)
                        .context("Unable to create rough bitmap buf")?,
                );
                let mut height_image = DynamicImage::ImageLuma8(
                    Self::scalar_ref_into_gray_image(&height_ref, &project_dir, &src_dir, 0)
                        .context("Unable to create height bitmap buf")?,
                );
                let mut transmission_image = DynamicImage::ImageLuma8(
                    Self::scalar_ref_into_gray_image(&transmission, &project_dir, &src_dir, 0)
                        .context("Unable to create transmission bitmap buf")?,
                );

                let width = metal_image
                    .width()
                    .max(rough_image.width())
                    .max(height_image.width())
                    .max(transmission_image.width());
                let height = metal_image
                    .height()
                    .max(rough_image.height())
                    .max(height_image.height())
                    .max(transmission_image.height());

                if metal_image.width() != width || metal_image.height() != height {
                    let filter_ty = if metal_image.width() == 1 && metal_image.height() == 1 {
                        FilterType::Nearest
                    } else {
                        FilterType::CatmullRom
                    };

                    metal_image = metal_image.resize_to_fill(width, height, filter_ty);
                }

                if rough_image.width() != width || rough_image.height() != height {
                    let filter_ty = if rough_image.width() == 1 && rough_image.height() == 1 {
                        FilterType::Nearest
                    } else {
                        FilterType::CatmullRom
                    };

                    rough_image = rough_image.resize_to_fill(width, height, filter_ty);
                }

                if height_image.width() != width || height_image.height() != height {
                    let filter_ty = if height_image.width() == 1 && height_image.height() == 1 {
                        FilterType::Nearest
                    } else {
                        FilterType::CatmullRom
                    };

                    height_image = height_image.resize_to_fill(width, height, filter_ty);
                }

                if transmission_image.width() != width || transmission_image.height() != height {
                    let filter_ty =
                        if transmission_image.width() == 1 && transmission_image.height() == 1 {
                            FilterType::Nearest
                        } else {
                            FilterType::CatmullRom
                        };

                    transmission_image =
                        transmission_image.resize_to_fill(width, height, filter_ty);
                }

                let mut params = Vec::with_capacity((4 * width * height) as usize);

                for y in 0..height {
                    for x in 0..width {
                        params.push(metal_image.get_pixel(x, y).0[0]);
                        params.push(rough_image.get_pixel(x, y).0[0]);
                        params.push(height_image.get_pixel(x, y).0[0]);
                        params.push(transmission_image.get_pixel(x, y).0[0]);
                    }
                }

                let mut writer = writer.lock();

                if let Some(id) = writer.ctx.get(&params_asset) {
                    id.as_bitmap().context("expected bitmap id for params")
                } else {
                    let params =
                        Bitmap::new(BitmapColor::Linear, BitmapFormat::Rgba, width, 1, params);
                    Ok(writer.push_bitmap(params, None))
                }
            })
        });

        let (color, emissive, normal, params) = rt
            .block_on(async move {
                let color = color.await.context("color task failed")??;
                let emissive = if let Some(emissive) = emissive {
                    Some(emissive.await.context("emissive task failed")??)
                } else {
                    None
                };
                let normal = if let Some(normal) = normal {
                    normal.await.context("normal task failed")??
                } else {
                    None
                };
                let params = if let Some(params) = params {
                    Some(params.await.context("params task failed")??)
                } else {
                    None
                };

                anyhow::Ok((color, emissive, normal, params))
            })
            .context("material bake tasks failed")?;

        Ok(MaterialInfo {
            color,
            emissive,
            normal,
            params,
            params_used,
        })
    }

    fn bake_normal_bitmap(
        bitmap: &mut BitmapAsset,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        path: Option<impl AsRef<Path>>,
    ) -> anyhow::Result<Option<BitmapId>> {
        let bitmap_buf = bitmap
            .as_bitmap_buf()
            .context("Unable to create normal bitmap buf")?;

        if Self::normal_bitmap_is_valid(&bitmap_buf) {
            bitmap.bake_from_path(writer, project_dir, path).map(Some)
        } else {
            if let Some(src) = bitmap.src() {
                warn!(
                    "Invalid normal map {}; treating material as having no normal map",
                    src.display()
                );
            } else {
                warn!("Invalid inline normal map; treating material as having no normal map");
            }

            Ok(None)
        }
    }

    fn normal_bitmap_is_valid(bitmap: &Bitmap) -> bool {
        if bitmap.format().byte_len() < BitmapFormat::Rgb.byte_len() {
            return false;
        }

        let mut avg = [0.0; 3];
        let mut count = 0.0;
        for pixel in bitmap.pixels().chunks(bitmap.format().byte_len()) {
            avg[0] += pixel[0] as f32;
            avg[1] += pixel[1] as f32;
            avg[2] += pixel[2] as f32;
            count += 1.0;
        }

        if count == 0.0 {
            return false;
        }

        avg[0] /= count;
        avg[1] /= count;
        avg[2] /= count;

        let decoded_avg = [
            avg[0] / 255.0 * 2.0 - 1.0,
            avg[1] / 255.0 * 2.0 - 1.0,
            avg[2] / 255.0 * 2.0 - 1.0,
        ];
        let decoded_avg_len_sq = decoded_avg[0] * decoded_avg[0]
            + decoded_avg[1] * decoded_avg[1]
            + decoded_avg[2] * decoded_avg[2];

        avg[2] > 16.0 && decoded_avg_len_sq > 0.01
    }

    fn scalar_ref_into_gray_image(
        scalar: &Option<ScalarRef>,
        project_dir: impl AsRef<Path>,
        src_dir: impl AsRef<Path>,
        default: u8,
    ) -> anyhow::Result<GrayImage> {
        let bitmap = match scalar {
            Some(ScalarRef::Asset(bitmap)) => bitmap
                .as_bitmap_buf()
                .context("Unable to create bitmap buf from scalar bitmap asset")?,
            Some(ScalarRef::Path(src)) => {
                if is_toml(src) {
                    let mut bitmap = Asset::read(src)?
                        .into_bitmap()
                        .context("Source file should be a bitmap asset")?;
                    bitmap.canonicalize(&project_dir, src_dir);
                    bitmap
                } else {
                    BitmapAsset::new(src)
                }
            }
            .as_bitmap_buf()
            .context("Unable to create bitmap buf")?,
            &Some(ScalarRef::Value(val)) => Bitmap::new(
                BitmapColor::Linear,
                BitmapFormat::R,
                1,
                1,
                [(val.0 * u8::MAX as f32) as _],
            ),
            None => Bitmap::new(BitmapColor::Linear, BitmapFormat::R, 1, 1, [default]),
        };
        let image = GrayImage::from_raw(bitmap.width(), bitmap.height(), bitmap.pixels().to_vec())
            .context("unable to create gray image from bitmap")?;

        Ok(image)
    }
}

impl Canonicalize for MaterialAsset {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        if let Some(color) = self.color.as_mut() {
            color.canonicalize(&project_dir, &src_dir);
        }

        if let Some(height) = self.height.as_mut() {
            height.canonicalize(&project_dir, &src_dir);
        }

        if let Some(emissive) = self.emissive.as_mut() {
            emissive.canonicalize(&project_dir, &src_dir);
        }

        if let Some(metal) = self.metal.as_mut() {
            metal.canonicalize(&project_dir, &src_dir);
        }

        if let Some(normal) = self.normal.as_mut() {
            normal.canonicalize(&project_dir, &src_dir);
        }

        if let Some(rough) = self.rough.as_mut() {
            rough.canonicalize(&project_dir, &src_dir);
        }

        if let Some(transmission) = self.transmission.as_mut() {
            transmission.canonicalize(&project_dir, &src_dir);
        }
    }
}

/// Holds a description of data used while baking materials. This is for caching.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct MaterialParams {
    #[serde(default, deserialize_with = "ScalarRef::de")]
    pub height: Option<ScalarRef>,

    /// A `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a single
    /// normalized value.
    #[serde(default, deserialize_with = "ScalarRef::de")]
    pub metal: Option<ScalarRef>,

    /// A `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a single
    /// normalized value.
    #[serde(default, deserialize_with = "ScalarRef::de")]
    pub rough: Option<ScalarRef>,

    /// A `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a single
    /// normalized value.
    #[serde(default, deserialize_with = "ScalarRef::de")]
    pub transmission: Option<ScalarRef>,
}

/// A reference to a bitmap asset, bitmap asset file, or three channel image source file.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum NormalRef {
    /// A `Bitmap` asset specified inline.
    Asset(BitmapAsset),

    /// A `Bitmap` asset file or three channel image source file.
    Path(PathBuf),
}

impl NormalRef {
    /// Deserialize from any of absent or:
    ///
    /// src of file.png:
    /// .. = "file.png"
    ///
    /// src of file.toml which must be a Bitmap asset:
    /// .. = "file.toml"
    ///
    /// src of a Bitmap asset:
    /// .. = { src = "file.png", format = "rgb" }
    fn de<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct NormalRefVisitor;

        impl<'de> Visitor<'de> for NormalRefVisitor {
            type Value = Option<NormalRef>;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter.write_str("path string or bitmap asset")
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let asset = Deserialize::deserialize(MapAccessDeserializer::new(map))?;

                Ok(Some(NormalRef::Asset(asset)))
            }

            fn visit_str<E>(self, str: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                Ok(Some(NormalRef::Path(PathBuf::from(str))))
            }
        }

        deserializer.deserialize_any(NormalRefVisitor)
    }
}

impl Canonicalize for NormalRef {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        match self {
            Self::Asset(bitmap) => bitmap.canonicalize(project_dir, src_dir),
            Self::Path(src) => *src = Self::canonicalize_project_path(project_dir, src_dir, &src),
        }
    }
}

/// Reference to a `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a
/// single value.
#[derive(Clone, Debug, Eq, Hash, PartialEq)]
pub enum ScalarRef {
    /// A `Bitmap` asset specified inline.
    Asset(BitmapAsset),

    /// A `Bitmap` asset file or single channel image source file.
    Path(PathBuf),

    /// A single value.
    Value(OrderedFloat<f32>),
}

impl ScalarRef {
    /// Deserialize from any of absent or:
    ///
    /// val of 1.0:
    /// .. = "#f"
    /// .. = "#ff"
    /// .. = 1.0
    ///
    /// src of file.png:
    /// .. = "file.png"
    ///
    /// src of file.toml which must be a Bitmap asset:
    /// .. = "file.toml"
    ///
    /// src of a Bitmap asset:
    /// .. = { src = "file.png", format = "r" }
    fn de<'de, D>(deserializer: D) -> Result<Option<Self>, D::Error>
    where
        D: Deserializer<'de>,
    {
        struct ScalarRefVisitor;

        impl<'de> Visitor<'de> for ScalarRefVisitor {
            type Value = Option<ScalarRef>;

            fn expecting(&self, formatter: &mut Formatter) -> std::fmt::Result {
                formatter
                    .write_str("hex string, path string, bitmap asset, or floating point value")
            }

            fn visit_f64<E>(self, val: f64) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                let val = val as f32;
                match val.classify() {
                    FpCategory::Zero | FpCategory::Normal if (0.0..=1.0).contains(&val) => (),
                    _ => return Err(E::custom("expected a scalar value between 0.0 and 1.0")),
                }

                Ok(Some(ScalarRef::Value(OrderedFloat(val))))
            }

            fn visit_map<M>(self, map: M) -> Result<Self::Value, M::Error>
            where
                M: MapAccess<'de>,
            {
                let asset = Deserialize::deserialize(MapAccessDeserializer::new(map))?;

                Ok(Some(ScalarRef::Asset(asset)))
            }

            fn visit_str<E>(self, str: &str) -> Result<Self::Value, E>
            where
                E: serde::de::Error,
            {
                if str.starts_with('#')
                    && let Some(val) = parse_hex_scalar(str)
                {
                    return Ok(Some(ScalarRef::Value(OrderedFloat(
                        val as f32 / u8::MAX as f32,
                    ))));
                }

                Ok(Some(ScalarRef::Path(PathBuf::from(str))))
            }
        }

        deserializer.deserialize_any(ScalarRefVisitor)
    }
}

impl Canonicalize for ScalarRef {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        match self {
            Self::Asset(bitmap) => bitmap.canonicalize(project_dir, src_dir),
            Self::Path(src) => *src = Self::canonicalize_project_path(project_dir, src_dir, &src),
            _ => (),
        }
    }
}

#[cfg(test)]
mod tests {
    use {
        super::{MaterialAsset, ScalarRef},
        ordered_float::OrderedFloat,
    };

    #[test]
    fn deserializes_height_scalar_value() {
        let material = toml::from_str::<MaterialAsset>("height = 0.5")
            .expect("height scalar should deserialize");

        assert_eq!(
            material.height,
            Some(ScalarRef::Value(OrderedFloat(0.5f32)))
        );
    }

    #[test]
    fn deserializes_transmission_scalar_value() {
        let material = toml::from_str::<MaterialAsset>("transmission = 0.5")
            .expect("transmission scalar should deserialize");

        assert_eq!(
            material.transmission,
            Some(ScalarRef::Value(OrderedFloat(0.5f32)))
        );
    }

    #[test]
    fn rejects_negative_color_values() {
        let err = toml::from_str::<MaterialAsset>("color = [-0.1, 0.0, 1.0]")
            .expect_err("negative color channel should be rejected");

        assert!(err.to_string().contains("between 0.0 and 1.0"));
    }

    #[test]
    fn rejects_negative_emissive_values() {
        let err = toml::from_str::<MaterialAsset>("emissive = [0.0, -0.1, 1.0]")
            .expect_err("negative emissive channel should be rejected");

        assert!(err.to_string().contains("between 0.0 and 1.0"));
    }

    #[test]
    fn rejects_negative_scalar_values() {
        let err = toml::from_str::<MaterialAsset>("metal = -0.1")
            .expect_err("negative scalar value should be rejected");

        assert!(err.to_string().contains("between 0.0 and 1.0"));
    }
}
