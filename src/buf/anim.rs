use {
    super::{Asset, Canonicalize, Writer, file_key},
    crate::{
        AnimationId,
        anim::{Animation, Channel, Outputs},
    },
    gltf::{
        animation::{
            Interpolation as GltfInterpolation,
            util::{ReadOutputs, Rotations},
        },
        import,
    },
    log::{Level::Debug, debug, info, log_enabled, warn},
    parking_lot::Mutex,
    serde::Deserialize,
    std::{
        collections::HashSet,
        path::{Path, PathBuf},
        sync::Arc,
        time::Duration,
    },
};

/// Holds a description of `.glb` or `.gltf` mesh animations.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct AnimationAsset {
    name: Option<String>,
    src: Option<PathBuf>,

    // Tables must follow values
    exclude: Option<Vec<String>>,
}

impl AnimationAsset {
    /// Reads and processes animation source files into an existing `.pak` file buffer.
    #[allow(unused)]
    pub(super) fn bake(
        &self,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        path: impl AsRef<Path>,
    ) -> anyhow::Result<AnimationId> {
        let Some(src) = self.src() else {
            return Err(anyhow::Error::msg("unspecified animation source"));
        };

        let asset = self.clone().into();

        if let Some(h) = writer.lock().ctx.get(&asset) {
            return Ok(h.as_animation().unwrap());
        }

        let key = file_key(&project_dir, &path);
        info!("Baking animation: {}", &key);

        let name = self.name();
        let (doc, bufs, _) = import(src).unwrap();

        if log_enabled!(Debug) {
            for anim in doc.animations() {
                debug!("Found animation '{}'", anim.name().unwrap_or_default());
            }
        }

        let mut anim = doc.animations().find(|anim| name == anim.name());
        if anim.is_none() && name.is_none() {
            anim = doc.animations().next();
        }

        let anim = anim.unwrap();
        let exclude = self
            .exclude()
            .unwrap_or_default()
            .iter()
            .map(|s| s.as_str())
            .collect::<HashSet<_>>();
        let mut channels = vec![];
        let mut channels_used = HashSet::new();

        'channel: for channel in anim.channels() {
            let name = if let Some(name) = channel.target().node().name() {
                name
            } else {
                warn!("Unnamed channel");

                continue;
            };

            if exclude.contains(name) {
                continue;
            }

            let data = channel.reader(|buf| bufs.get(buf.index()).map(|data| &*data.0));
            let inputs = data
                .read_inputs()
                .unwrap()
                .map(|input| Duration::from_secs_f32(input).as_millis() as u32)
                .collect::<Vec<_>>();
            if inputs.is_empty() {
                warn!("Empty channel data");

                continue;
            }

            // Assure increasing sort
            {
                let mut input = inputs[0];
                for val in inputs.iter().skip(1).copied() {
                    if val > input {
                        input = val
                    } else {
                        warn!("Unsorted input data");

                        continue 'channel;
                    }
                }
            }

            let outputs = match data.read_outputs().unwrap() {
                ReadOutputs::Rotations(Rotations::F32(rotations)) => {
                    Outputs::Rotations(rotations.collect())
                }
                ReadOutputs::Scales(scales) => Outputs::Scales(scales.collect()),
                ReadOutputs::Translations(translations) => {
                    Outputs::Translations(translations.collect())
                }
                _ => {
                    warn!("Unsupported morph target channel");

                    continue;
                }
            };

            #[derive(Eq, Hash, PartialEq)]
            enum ChannelType {
                Rotation,
                Scale,
                Translation,
            }

            let channel_ty = match &outputs {
                Outputs::Rotations(rotations) => ChannelType::Rotation,
                Outputs::Scales(scales) => ChannelType::Scale,
                Outputs::Translations(translations) => ChannelType::Translation,
            };

            if !channels_used.insert((name, channel_ty)) {
                warn!("Duplicate channels found");

                continue;
            }

            let outputs_len = match &outputs {
                Outputs::Rotations(rotations) => rotations.len(),
                Outputs::Scales(scales) => scales.len(),
                Outputs::Translations(translations) => translations.len(),
            };
            let sampler = channel.sampler();
            let interpolation = sampler.interpolation();
            let expected_outputs = match interpolation {
                GltfInterpolation::Linear | GltfInterpolation::Step => inputs.len(),
                GltfInterpolation::CubicSpline => inputs.len() * 3,
            };

            if outputs_len != expected_outputs {
                warn!("Invalid output data");

                continue;
            }

            channels.push(Channel::new(name, interpolation, inputs, outputs));
        }

        let mut writer = writer.lock();
        if let Some(id) = writer.ctx.get(&asset) {
            return Ok(id.as_animation().unwrap());
        }

        let id = writer.push_animation(Animation::new(channels), Some(key));
        writer.ctx.insert(asset, id.into());

        Ok(id)
    }

    /// The bones which were excluded when reading the animation file.
    #[allow(unused)]
    pub fn exclude(&self) -> Option<&[String]> {
        self.exclude.as_deref()
    }

    /// The name of the animation within the animation file.
    #[allow(unused)]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Sets the mesh file source.
    pub fn set_src(&mut self, src: impl AsRef<Path>) {
        self.src = Some(src.as_ref().to_path_buf());
    }

    /// The animation file source.
    #[allow(unused)]
    pub fn src(&self) -> Option<&Path> {
        self.src.as_deref()
    }
}

impl Canonicalize for AnimationAsset {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        if let Some(src) = self.src() {
            self.src = Some(Self::canonicalize_project_path(project_dir, src_dir, src));
        }
    }
}

impl From<AnimationAsset> for Asset {
    fn from(anim: AnimationAsset) -> Self {
        Self::Animation(anim)
    }
}
