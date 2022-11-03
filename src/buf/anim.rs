use {
    super::{
        super::anim::{AnimationBuf, Channel},
        file_key, AnimationId, Asset, Canonicalize, Writer,
    },
    glam::{quat, Quat, Vec3},
    gltf::{
        animation::{
            util::{ReadOutputs, Rotations},
            Property,
        },
        import,
    },
    log::{info, warn},
    parking_lot::Mutex,
    serde::Deserialize,
    std::{
        collections::{hash_map::RandomState, HashSet},
        path::{Path, PathBuf},
        sync::Arc,
    },
};

/// Holds a description of `.glb` or `.gltf` model animations.
#[derive(Clone, Debug, Deserialize, Eq, Hash, PartialEq)]
pub struct Animation {
    name: Option<String>,
    src: PathBuf,

    // Tables must follow values
    exclude: Option<Vec<String>>,
}

impl Animation {
    /// Reads and processes animation source files into an existing `.pak` file buffer.
    #[allow(unused)]
    pub(super) fn bake(
        &self,
        writer: &Arc<Mutex<Writer>>,
        project_dir: impl AsRef<Path>,
        path: impl AsRef<Path>,
    ) -> anyhow::Result<AnimationId> {
        let asset = self.clone().into();

        if let Some(h) = writer.lock().ctx.get(&asset) {
            return Ok(h.as_animation().unwrap());
        }

        let key = file_key(&project_dir, &path);
        info!("Baking animation: {}", &key);

        //let src_dir = src.as_ref().parent().unwrap();
        let src = self.src(); // TODO get_path(&dir, asset.src(), project_dir);

        let name = self.name();
        let (doc, bufs, _) = import(src).unwrap();
        let mut anim = doc.animations().find(|anim| name == anim.name());
        if anim.is_none() && name.is_none() && doc.animations().count() > 0 {
            anim = doc.animations().next();
        }

        let anim = anim.unwrap();
        let exclude: HashSet<&str, RandomState> = self
            .exclude()
            .unwrap_or_default()
            .iter()
            .map(|s| s.as_str())
            .collect();

        #[allow(unused)]
        enum Output {
            Rotations(Vec<Quat>),
            Scales(Vec<Vec3>),
            Translations(Vec<Vec3>),
        }

        let mut channels = vec![];
        let mut channel_names = HashSet::new();

        'channel: for channel in anim.channels() {
            let name = if let Some(name) = channel.target().node().name() {
                name
            } else {
                continue;
            };

            if exclude.contains(name) {
                continue;
            }

            // Only support rotations for now
            let property = channel.target().property();
            match property {
                Property::Rotation => (),
                _ => continue,
            }

            // We require all joint names to be unique
            if channel_names.contains(&name) {
                warn!("Duplicate rotation channels or non-unique targets");
                continue;
            }

            channel_names.insert(name);

            let sampler = channel.sampler();
            let interpolation = sampler.interpolation();

            let data = channel.reader(|buf| bufs.get(buf.index()).map(|data| &*data.0));
            let inputs = data.read_inputs().unwrap().collect::<Vec<_>>();
            if inputs.is_empty() {
                continue;
            }

            // Assure increasing sort
            let mut input = inputs[0];
            for val in inputs.iter().skip(1) {
                if *val > input {
                    input = *val
                } else {
                    warn!("Unsorted input data");
                    continue 'channel;
                }
            }

            let outputs = match data.read_outputs().unwrap() {
                ReadOutputs::Rotations(Rotations::F32(rotations)) => {
                    Output::Rotations(rotations.map(|r| quat(r[0], r[1], r[2], r[3])).collect())
                }
                _ => continue,
            };
            let rotations = match outputs {
                Output::Rotations(r) => r,
                _ => continue,
            };

            channels.push(Channel::new(name, interpolation, inputs, rotations));

            // print!(
            //     " {} {:#?}",
            //     channel.target().node().name().unwrap_or("?"),
            //     channel.target().property()
            // );
            // print!(
            //     " ({:#?} {} Inputs, {} Output ",
            //     interpolation,
            //     inputs.len(),
            //     //inputs.iter().rev().take(5).collect::<Vec<_>>(),
            //     match &output {
            //         Output::Rotations(r) => r.len(),
            //         Output::Scales(s) => s.len(),
            //         Output::Translations(t) => t.len(),
            //     }
            // );

            // match &output {
            //     Output::Rotations(_) => print!("Rotations"),
            //     Output::Scales(_) => print!("Scales"),
            //     Output::Translations(_) => print!("Translations"),
            // }

            // println!(")");
        }

        // Sort channels by name (they are all rotations)
        channels.sort_unstable_by(|a, b| a.target().cmp(b.target()));

        let mut writer = writer.lock();
        if let Some(id) = writer.ctx.get(&asset) {
            return Ok(id.as_animation().unwrap());
        }

        let id = writer.push_animation(AnimationBuf { channels }, Some(key));
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

    /// The animation file source.
    #[allow(unused)]
    pub fn src(&self) -> &Path {
        self.src.as_path()
    }
}

impl Canonicalize for Animation {
    fn canonicalize(&mut self, project_dir: impl AsRef<Path>, src_dir: impl AsRef<Path>) {
        self.src = Self::canonicalize_project_path(project_dir, src_dir, &self.src);
    }
}

impl From<Animation> for Asset {
    fn from(anim: Animation) -> Self {
        Self::Animation(anim)
    }
}
