use {
    super::{
        Asset, Canonicalize, Writer,
        bitmap::{BitmapAsset, BitmapSwizzle, MipQuality, MipSemantic},
        file_key, is_toml, parent, parse_hex_color, parse_hex_scalar,
    },
    crate::{
        BitmapId, MaterialId, MaterialInfo, MaterialParameterFlags,
        bitmap::{Bitmap, BitmapColor, BitmapCompression, BitmapFormat},
    },
    anyhow::{Context as _, bail, ensure},
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
        collections::BTreeMap,
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
                    if val[3] != u8::MAX {
                        return Err(E::custom("expected an emissive color without alpha"));
                    }

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
    /// Bake policy applied to final color, normal, emissive and packed parameter maps.
    pub mip_quality: MipQuality,

    /// Whether base-color alpha should reject fragments below the engine cutoff.
    pub alpha_test: bool,

    /// A `Bitmap` asset, `Bitmap` asset file, three or four channel image source file, or single
    /// four channel color.
    #[serde(deserialize_with = "ColorRef::de")]
    pub color: Option<ColorRef>,

    /// Whether the material uses landscape-specific surface shading.
    pub landscape: bool,

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
    /// normalized value. Shares the packed parameter B channel with `height`.
    #[serde(deserialize_with = "ScalarRef::de")]
    pub occlusion: Option<ScalarRef>,

    /// A `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a single
    /// normalized value.
    #[serde(deserialize_with = "ScalarRef::de")]
    pub rough: Option<ScalarRef>,

    /// A `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a single
    /// normalized value.
    #[serde(deserialize_with = "ScalarRef::de")]
    pub transmission: Option<ScalarRef>,

    /// Application-defined data, using the same value types as scene data.
    pub data: Option<BTreeMap<String, super::scene::Data>>,
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
        path: Option<impl AsRef<Path>>,
    ) -> anyhow::Result<MaterialId> {
        // Early-out if we have already baked this material
        let asset = self.clone().into();
        let key = path.as_ref().map(|path| file_key(&project_dir, path));
        if let Some(id) = writer.lock().asset_id(&asset, key.as_deref())? {
            return id
                .as_material()
                .context("asset context returned non-material id");
        }

        // If a source is given it will be available as a key inside the .pak (sources are not
        // given if the asset is specified inline - those are only available in the .pak via ID)
        if let Some(key) = &key {
            // This material will be accessible using this key
            info!("Baking material: {}", key);
        } else {
            // This material will only be accessible using the ID
            info!("Baking material: (inline)");
        }

        let material_info = Writer::with_asset_policy(writer, &asset, || {
            self.as_material_info(rt, writer, project_dir)
        })?;

        let mut writer = writer.lock();
        if let Some(id) = writer.asset_id(&asset, key.as_deref())? {
            return id
                .as_material()
                .context("asset context returned non-material id");
        }

        let id = writer.push_material(material_info);
        writer.commit_asset(asset, id, key)?;

        Ok(id)
    }

    fn as_material_info(
        &mut self,
        rt: &Runtime,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
    ) -> anyhow::Result<MaterialInfo> {
        if self.height.is_some() && self.occlusion.is_some() {
            bail!(
                "material cannot specify both height and occlusion because they share parameter B"
            );
        }

        let compress_textures = writer.lock().texture_compression();
        let quality = self.mip_quality;
        if quality == MipQuality::High {
            ensure!(
                !self.alpha_test,
                "high mip quality does not support alpha-test coverage preservation"
            );
            ensure!(
                self.height.is_none(),
                "high mip quality does not support height; retain the legacy single-level height policy"
            );
            ensure!(
                compress_textures,
                "high material mip quality requires texture-compression = true"
            );
        }
        let color = match &self.color {
            Some(ColorRef::Asset(bitmap)) => {
                let writer = writer.clone();
                let project_dir = project_dir.as_ref().to_path_buf();
                let bitmap = bitmap
                    .clone()
                    .with_material_quality(quality, MipSemantic::Color);
                ensure!(
                    !self.alpha_test || bitmap.mip_quality() != MipQuality::High,
                    "high mip quality does not support alpha-test coverage preservation"
                );

                rt.spawn_blocking(move || {
                    bitmap
                        .with_default_color_compression_if(compress_textures)
                        .bake(&writer, &project_dir)
                        .context("Unable to bake color asset bitmap")
                })
            }
            Some(ColorRef::Path(src)) => {
                let bitmap = if is_toml(src) {
                    let mut bitmap = Asset::read(src)
                        .context("Unable to read color bitmap asset")?
                        .into_bitmap()
                        .context("Source file should be a bitmap asset")?;
                    bitmap.canonicalize(&project_dir, parent(src));
                    bitmap
                } else {
                    BitmapAsset::new(src)
                };
                let bitmap = bitmap.with_material_quality(quality, MipSemantic::Color);
                ensure!(
                    !self.alpha_test || bitmap.mip_quality() != MipQuality::High,
                    "high mip quality does not support alpha-test coverage preservation"
                );
                let writer = writer.clone();
                let project_dir = project_dir.as_ref().to_path_buf();

                rt.spawn_blocking(move || {
                    bitmap
                        .with_default_color_compression_if(compress_textures)
                        .bake_from_path(&writer, &project_dir, Option::<PathBuf>::None)
                        .context("Unable to bake color asset bitmap from path")
                })
            }
            &Some(ColorRef::Value(val)) => {
                let writer = writer.clone();

                rt.spawn_blocking(move || -> anyhow::Result<BitmapId> {
                    let mut writer = writer.lock();
                    let asset = Asset::ColorRgba(val, quality);
                    if let Some(id) = writer.asset_id(&asset, None)? {
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
                        let bitmap = Self::compress_constant(bitmap, quality);
                        let policy = writer.policy_for(&asset);
                        let id = writer.push_bitmap(bitmap, policy)?;
                        writer.commit_asset(asset, id, None)?;
                        Ok(id)
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
                    let asset = Asset::ColorRgba(potters_clay, quality);
                    if let Some(id) = writer.asset_id(&asset, None)? {
                        id.as_bitmap()
                            .context("expected bitmap id for default color")
                    } else {
                        let bitmap = Bitmap::new(
                            BitmapColor::Linear,
                            BitmapFormat::Rgba,
                            1,
                            1,
                            [
                                (potters_clay[0].0 * u8::MAX as f32) as u8,
                                (potters_clay[1].0 * u8::MAX as f32) as u8,
                                (potters_clay[2].0 * u8::MAX as f32) as u8,
                                (potters_clay[3].0 * u8::MAX as f32) as u8,
                            ],
                        );
                        let bitmap = Self::compress_constant(bitmap, quality);
                        let policy = writer.policy_for(&asset);
                        let id = writer.push_bitmap(bitmap, policy)?;
                        writer.commit_asset(asset, id, None)?;
                        Ok(id)
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
                        let mut bitmap = bitmap
                            .clone()
                            .with_material_quality(quality, MipSemantic::Normal)
                            .with_color(BitmapColor::Linear)
                            .with_swizzle(BitmapSwizzle::RGB)
                            .with_default_compression_if(compress_textures, BitmapCompression::Bc5);

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
                            bitmap.canonicalize(&project_dir, parent(src));
                            bitmap
                        } else {
                            BitmapAsset::new(src)
                        };
                        let writer = writer.clone();
                        let project_dir = project_dir.as_ref().to_path_buf();

                        rt.spawn_blocking(move || {
                            bitmap = bitmap
                                .with_material_quality(quality, MipSemantic::Normal)
                                .with_color(BitmapColor::Linear)
                                .with_swizzle(BitmapSwizzle::RGB)
                                .with_default_compression_if(
                                    compress_textures,
                                    BitmapCompression::Bc5,
                                );
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
                        let bitmap = bitmap
                            .clone()
                            .with_swizzle(BitmapSwizzle::RGB)
                            .with_material_quality(quality, MipSemantic::Color);

                        rt.spawn_blocking(move || -> anyhow::Result<BitmapId> {
                            bitmap
                                .with_default_color_compression_if(compress_textures)
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
                            bitmap.canonicalize(&project_dir, parent(src));
                            bitmap
                        } else {
                            BitmapAsset::new(src)
                        };
                        let writer = writer.clone();
                        let project_dir = project_dir.as_ref().to_path_buf();

                        rt.spawn_blocking(move || -> anyhow::Result<BitmapId> {
                            bitmap
                                .with_swizzle(BitmapSwizzle::RGB)
                                .with_material_quality(quality, MipSemantic::Color)
                                .with_default_color_compression_if(compress_textures)
                                .bake_from_path(&writer, &project_dir, Option::<PathBuf>::None)
                                .context("Unable to bake emissive asset bitmap from path")
                        })
                    }
                    EmissiveRef::Value(val) => {
                        let writer = writer.clone();
                        let val = *val;

                        rt.spawn_blocking(move || -> anyhow::Result<BitmapId> {
                            let mut writer = writer.lock();
                            let asset = Asset::ColorRgb(val, quality);
                            if let Some(id) = writer.asset_id(&asset, None)? {
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
                                let bitmap = Self::compress_constant(bitmap, quality);
                                let policy = writer.policy_for(&asset);
                                let id = writer.push_bitmap(bitmap, policy)?;
                                writer.commit_asset(asset, id, None)?;
                                Ok(id)
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
        if self.occlusion.is_some() {
            params_used |= MaterialParameterFlags::OCCLUSION;
        }
        if self.transmission.is_some() {
            params_used |= MaterialParameterFlags::TRANSMISSION;
        }
        let height_ref = self.height.clone();
        let metal = self.metal.clone();
        let occlusion = self.occlusion.clone();
        let rough = self.rough.clone();
        let transmission = self.transmission.clone();
        let params_asset = Asset::MaterialParams(MaterialParams {
            mip_quality: quality,
            height: height_ref,
            metal,
            occlusion,
            rough,
            transmission,
        });
        let use_params = !params_used.is_empty();
        if self.landscape {
            params_used |= MaterialParameterFlags::LANDSCAPE;
        }
        let params = use_params.then(|| {
            let project_dir = project_dir.as_ref().to_path_buf();
            let writer = writer.clone();
            let height_ref = self.height.clone();
            let preserve_height = height_ref.is_some();
            let metal = self.metal.clone();
            let occlusion = self.occlusion.clone();
            let rough = self.rough.clone();
            let transmission = self.transmission.clone();

            rt.spawn_blocking(move || {
                if let Some(id) = writer.lock().asset_id(&params_asset, None)? {
                    return id.as_bitmap().context("expected bitmap id for params");
                }

                let mut metal_image = DynamicImage::ImageLuma8(
                    Self::scalar_ref_into_gray_image(&metal, &project_dir, 0, quality)
                        .context("Unable to create metal bitmap buf")?,
                );
                let mut rough_image = DynamicImage::ImageLuma8(
                    Self::scalar_ref_into_gray_image(&rough, &project_dir, u8::MAX, quality)
                        .context("Unable to create rough bitmap buf")?,
                );
                let parameter_b_default = if occlusion.is_some() { u8::MAX } else { 0 };
                let parameter_b = height_ref.or(occlusion);
                let mut parameter_b_image = DynamicImage::ImageLuma8(
                    Self::scalar_ref_into_gray_image(
                        &parameter_b,
                        &project_dir,
                        parameter_b_default,
                        quality,
                    )
                    .context("Unable to create height or occlusion bitmap buf")?,
                );
                let mut transmission_image = DynamicImage::ImageLuma8(
                    Self::scalar_ref_into_gray_image(&transmission, &project_dir, 0, quality)
                        .context("Unable to create transmission bitmap buf")?,
                );

                let width = metal_image
                    .width()
                    .max(rough_image.width())
                    .max(parameter_b_image.width())
                    .max(transmission_image.width());
                let height = metal_image
                    .height()
                    .max(rough_image.height())
                    .max(parameter_b_image.height())
                    .max(transmission_image.height());

                if quality == MipQuality::High {
                    for image in [&metal_image, &rough_image, &parameter_b_image, &transmission_image] {
                        ensure!(image.dimensions() == (width, height) || image.dimensions() == (1, 1),
                            "high packed parameter inputs must share native dimensions (or be 1x1 constants); resize is unsupported");
                    }
                }

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

                if parameter_b_image.width() != width || parameter_b_image.height() != height {
                    let filter_ty =
                        if parameter_b_image.width() == 1 && parameter_b_image.height() == 1 {
                            FilterType::Nearest
                        } else {
                            FilterType::CatmullRom
                        };

                    parameter_b_image = parameter_b_image.resize_to_fill(width, height, filter_ty);
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
                        params.push(parameter_b_image.get_pixel(x, y).0[0]);
                        params.push(transmission_image.get_pixel(x, y).0[0]);
                    }
                }

                let mut writer = writer.lock();

                if let Some(id) = writer.asset_id(&params_asset, None)? {
                    id.as_bitmap().context("expected bitmap id for params")
                } else {
                    let mip_levels = if compress_textures && !preserve_height {
                        u32::BITS - width.max(height).leading_zeros()
                    } else {
                        1
                    };
                    let params = Bitmap::new(
                        BitmapColor::Linear,
                        BitmapFormat::Rgba,
                        width,
                        mip_levels,
                        params,
                    );
                    // BC1 supplies an implicit alpha of one, which would turn absent
                    // transmission into full transmission in consumers of the packed map.
                    let compression = BitmapCompression::Bc3;
                    // BC3 color endpoints couple RGB channels and visibly leak
                    // roughness into an independently authored height map.
                    let params = if compress_textures && !preserve_height {
                        let compressed = if quality == MipQuality::High {
                            BitmapAsset::compress_with_quality(&params, compression, quality, MipSemantic::Data)
                        } else {
                            BitmapAsset::compress(&params, compression)
                        };
                        params.with_compressed(compressed)
                    } else {
                        params
                    };
                    let policy = writer.policy_for(&params_asset);
                    let id = writer.push_bitmap(params, policy)?;
                    writer.commit_asset(params_asset, id, None)?;
                    Ok(id)
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
            alpha_test: self.alpha_test,
            color,
            emissive,
            normal,
            params,
            params_used,
            data: self
                .data
                .iter()
                .flat_map(|data| data.iter())
                .map(|(key, value)| (key.clone(), value.clone().into()))
                .collect(),
        })
    }

    fn compress_constant(bitmap: Bitmap, quality: MipQuality) -> Bitmap {
        if quality == MipQuality::High {
            let compression = if bitmap.format().has_alpha() {
                BitmapCompression::Bc3
            } else {
                BitmapCompression::Bc1Rgb
            };
            let compressed = BitmapAsset::compress_with_quality(
                &bitmap,
                compression,
                quality,
                MipSemantic::Color,
            );
            bitmap.with_compressed(compressed)
        } else {
            bitmap
        }
    }

    fn bake_normal_bitmap(
        bitmap: &mut BitmapAsset,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        path: Option<impl AsRef<Path>>,
    ) -> anyhow::Result<Option<BitmapId>> {
        // High BC5 reconstructs positive Z from RG; source B and average direction
        // are not validity criteria (a cancelling vector field is valid).
        if bitmap.mip_quality() == MipQuality::High {
            return bitmap.bake_from_path(writer, project_dir, path).map(Some);
        }

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
        default: u8,
        quality: MipQuality,
    ) -> anyhow::Result<GrayImage> {
        let bitmap = match scalar {
            Some(ScalarRef::Asset(bitmap)) => bitmap
                .scalar_pixels(quality)
                .context("Unable to create bitmap buf from scalar bitmap asset")?,
            Some(ScalarRef::Path(src)) => {
                if is_toml(src) {
                    let mut bitmap = Asset::read(src)?
                        .into_bitmap()
                        .context("Source file should be a bitmap asset")?;
                    bitmap.canonicalize(&project_dir, parent(src));
                    bitmap
                } else {
                    BitmapAsset::new(src)
                }
            }
            .scalar_pixels(quality)
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
        let pixels = bitmap.pixels_as_format(BitmapFormat::R).collect::<Vec<_>>();
        let image = GrayImage::from_raw(bitmap.width(), bitmap.height(), pixels)
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

        if let Some(occlusion) = self.occlusion.as_mut() {
            occlusion.canonicalize(&project_dir, &src_dir);
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
    #[serde(default, rename = "mip-quality")]
    pub mip_quality: MipQuality,

    #[serde(default, deserialize_with = "ScalarRef::de")]
    pub height: Option<ScalarRef>,

    /// A `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a single
    /// normalized value.
    #[serde(default, deserialize_with = "ScalarRef::de")]
    pub metal: Option<ScalarRef>,

    /// A `Bitmap` asset, `Bitmap` asset file, single channel image source file, or a single
    /// normalized value.
    #[serde(default, deserialize_with = "ScalarRef::de")]
    pub occlusion: Option<ScalarRef>,

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
mod test {
    use {
        super::*,
        crate::{Pak as _, PakBuf, buf::writer::StoragePolicy},
        ordered_float::OrderedFloat,
    };

    #[test]
    fn high_material_propagates_final_policies_and_separates_shared_assets() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        image::RgbImage::from_fn(7, 3, |x, y| {
            image::Rgb([(x * 40) as u8, (y * 90) as u8, 200])
        })
        .save(root.join("source.png"))
        .unwrap();
        std::fs::write(
            root.join("source.toml"),
            "[bitmap]\nsrc = 'source.png'\ncolor = 'linear'\ncompression = 'bc5'\n",
        )
        .unwrap();
        let rt = Runtime::new().unwrap();
        let writer = Arc::new(Mutex::new(Writer::default()));
        writer.lock().set_active_policy(StoragePolicy {
            texture_compression: true,
            ..Default::default()
        });
        let recipe = "color = { src = 'source.png' }\nnormal = 'source.toml'\nemissive = 'source.png'\nmetal = { src = 'source.png', swizzle = 'r' }\nrough = { src = 'source.png', swizzle = 'g' }\nocclusion = { src = 'source.png', swizzle = 'b' }\ntransmission = 0.25\n";
        let mut legacy: MaterialAsset = toml::from_str(recipe).unwrap();
        assert_eq!(legacy.mip_quality, MipQuality::Default);
        legacy.canonicalize(root, root);
        let mut high = legacy.clone();
        high.mip_quality = MipQuality::High;
        let a = legacy.as_material_info(&rt, &writer, root).unwrap();
        let b = high.as_material_info(&rt, &writer, root).unwrap();
        let mut inline = high.clone();
        inline.normal = Some(NormalRef::Asset(
            BitmapAsset::new(root.join("source.png"))
                .with_color(BitmapColor::Linear)
                .with_compression(BitmapCompression::Bc5),
        ));
        inline.emissive = Some(EmissiveRef::Asset(BitmapAsset::new(
            root.join("source.png"),
        )));
        inline.color = Some(ColorRef::Path(root.join("source.png")));
        let c = inline.as_material_info(&rt, &writer, root).unwrap();
        assert_eq!(b.normal, c.normal);
        assert_eq!(b.color, c.color);
        assert_eq!(b.emissive, c.emissive);
        assert_eq!(b.params, c.params);
        assert_ne!(a.params, b.params);
        assert_ne!(a.color, b.color);
        assert_ne!(a.normal, b.normal);
        assert_ne!(a.emissive, b.emissive);
        let standalone = BitmapAsset::new(root.join("source.png"))
            .with_color(BitmapColor::Linear)
            .with_swizzle(BitmapSwizzle::RGB)
            .with_compression(BitmapCompression::Bc5)
            .with_mip_quality(MipQuality::High)
            .bake(&writer, root)
            .unwrap();
        assert_ne!(Some(standalone), b.normal);
        let dst = root.join("test.pak");
        writer.lock().write(&dst).unwrap();
        let mut pak = PakBuf::open(dst).unwrap();
        for (legacy, high, semantic) in [
            (a.color, b.color, MipSemantic::Color),
            (a.emissive.unwrap(), b.emissive.unwrap(), MipSemantic::Color),
            (a.normal.unwrap(), b.normal.unwrap(), MipSemantic::Normal),
            (a.params.unwrap(), b.params.unwrap(), MipSemantic::Data),
        ] {
            let raw = pak.read_bitmap_id(high).unwrap();
            assert_eq!(raw.pixels(), pak.read_bitmap_id(legacy).unwrap().pixels());
            let compressed = pak.read_compressed_bitmap_id(high).unwrap().unwrap();
            assert_eq!(
                compressed
                    .mips()
                    .iter()
                    .map(|mip| mip.extent())
                    .collect::<Vec<_>>(),
                [(7, 3), (3, 1), (1, 1)]
            );
            let expected = BitmapAsset::compress_with_quality(
                &raw,
                compressed.format(),
                MipQuality::High,
                semantic,
            );
            for (actual, expected) in compressed.mips().iter().zip(expected.mips()) {
                assert_eq!(actual.bytes(), expected.bytes());
            }
            let legacy_raw = pak.read_bitmap_id(legacy).unwrap();
            let legacy_compressed = pak.read_compressed_bitmap_id(legacy).unwrap().unwrap();
            let expected = BitmapAsset::compress(&legacy_raw, legacy_compressed.format());
            for (actual, expected) in legacy_compressed.mips().iter().zip(expected.mips()) {
                assert_eq!(actual.bytes(), expected.bytes());
            }
            if semantic == MipSemantic::Data {
                assert_eq!(&raw.pixels()[..4], &[0, 0, 200, 63]);
                let mut decoded = [0; 4];
                texpresso::Format::Bc3.decompress(
                    compressed.mips().last().unwrap().bytes(),
                    1,
                    1,
                    &mut decoded,
                );
                for (value, expected) in decoded.into_iter().zip([120_i16, 90, 200, 63]) {
                    assert!((i16::from(value) - expected).abs() <= 8);
                }
            }
        }
    }

    #[test]
    fn high_material_limits_and_scalar_policy_are_explicit() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path();
        let rt = Runtime::new().unwrap();
        assert!(toml::from_str::<MaterialAsset>("mip-quality = 'unknown'").is_err());
        for (recipe, compress, message) in [
            ("alpha-test = true", true, "alpha-test"),
            ("height = 0.5", true, "height"),
            ("rough = 0.5", false, "requires texture-compression"),
            (
                "rough = { src = 'missing.png', resize = 4 }",
                true,
                "retain native",
            ),
            (
                "rough = { src = 'missing.png', mip-quality = 'high' }",
                true,
                "material root",
            ),
            (
                "normal = { src = 'missing.png', compression = 'bc3' }",
                true,
                "require bc5",
            ),
        ] {
            let writer = Arc::new(Mutex::new(Writer::default()));
            writer.lock().set_active_policy(StoragePolicy {
                texture_compression: compress,
                ..Default::default()
            });
            let mut material: MaterialAsset =
                toml::from_str(&format!("mip-quality = 'high'\n{recipe}")).unwrap();
            let error = material.as_material_info(&rt, &writer, root).unwrap_err();
            assert!(format!("{error:#}").contains(message), "{error:#}");
        }
        std::fs::write(
            root.join("scalar.toml"),
            "[bitmap]\nsrc = 'missing.png'\nmip-quality = 'high'\n",
        )
        .unwrap();
        let scalar = Some(ScalarRef::Path(root.join("scalar.toml")));
        assert!(
            format!(
                "{:#}",
                MaterialAsset::scalar_ref_into_gray_image(&scalar, root, 0, MipQuality::High)
                    .unwrap_err()
            )
            .contains("material root")
        );
        image::GrayImage::new(2, 1)
            .save(root.join("small.png"))
            .unwrap();
        image::GrayImage::new(3, 1)
            .save(root.join("large.png"))
            .unwrap();
        let writer = Arc::new(Mutex::new(Writer::default()));
        writer.lock().set_active_policy(StoragePolicy {
            texture_compression: true,
            ..Default::default()
        });
        let mut material: MaterialAsset =
            toml::from_str("mip-quality = 'high'\nmetal = 'small.png'\nrough = 'large.png'")
                .unwrap();
        material.canonicalize(root, root);
        assert!(
            format!(
                "{:#}",
                material.as_material_info(&rt, &writer, root).unwrap_err()
            )
            .contains("share native dimensions")
        );
    }

    #[test]
    fn alpha_test_defaults_off_and_can_be_enabled() {
        let default = toml::from_str::<MaterialAsset>("").expect("material should deserialize");
        let enabled = toml::from_str::<MaterialAsset>("alpha-test = true")
            .expect("alpha-test should deserialize");

        assert!(!default.alpha_test);
        assert!(enabled.alpha_test);
    }

    #[test]
    fn data_defaults_to_absent() {
        let default = toml::from_str::<MaterialAsset>("").expect("material should deserialize");
        assert!(default.data.is_none());
    }

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
    fn deserializes_occlusion_path() {
        let material = toml::from_str::<MaterialAsset>("occlusion = 'textures/ao.png'")
            .expect("occlusion path should deserialize");

        assert_eq!(
            material.occlusion,
            Some(ScalarRef::Path("textures/ao.png".into()))
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
    fn rejects_emissive_hex_alpha() {
        let err = toml::from_str::<MaterialAsset>("emissive = '#abc0'")
            .expect_err("emissive alpha should be rejected");

        assert!(err.to_string().contains("without alpha"));
    }

    #[test]
    fn rejects_negative_scalar_values() {
        let err = toml::from_str::<MaterialAsset>("metal = -0.1")
            .expect_err("negative scalar value should be rejected");

        assert!(err.to_string().contains("between 0.0 and 1.0"));
    }
}
