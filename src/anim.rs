use {
    super::{Quat, Vec3},
    serde::{Deserialize, Deserializer, Serialize, de::Error},
};

#[cfg(feature = "bake")]
use gltf::animation::Interpolation as GltfInterpolation;

#[cfg(feature = "bake")]
use anyhow::bail;

/// Holds an `Animation` in a `.pak` file. For data transport only.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct Animation {
    channels: Vec<Channel>,
}

impl Animation {
    #[cfg(feature = "bake")]
    pub(super) fn new(channels: Vec<Channel>) -> Self {
        Self { channels }
    }

    /// The channels (joints/bones) of movement used in this `Animation`.
    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }
}

/// Describes the animation of one joint.
#[derive(Clone, Debug, PartialEq, Serialize)]
pub struct Channel {
    inputs: Vec<u32>,
    interpolation: Interpolation,
    outputs: Outputs,
    target: String,
}

impl<'de> Deserialize<'de> for Channel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct ChannelData {
            inputs: Vec<u32>,
            interpolation: Interpolation,
            outputs: Outputs,
            target: String,
        }

        let data = ChannelData::deserialize(deserializer)?;
        if data.target.is_empty() {
            return Err(D::Error::custom("channel target is empty"));
        }

        if data.inputs.is_empty() {
            return Err(D::Error::custom("channel has no inputs"));
        }

        Ok(Self {
            inputs: data.inputs,
            interpolation: data.interpolation,
            outputs: data.outputs,
            target: data.target,
        })
    }
}

impl Channel {
    #[cfg(feature = "bake")]
    pub(crate) fn new<T: AsRef<str>, I: IntoIterator<Item = u32>>(
        target: T,
        interpolation: GltfInterpolation,
        inputs: I,
        outputs: Outputs,
    ) -> anyhow::Result<Self> {
        let inputs = inputs.into_iter().collect::<Vec<_>>();
        let target = target.as_ref().to_owned();

        if target.is_empty() {
            bail!("channel target is empty");
        }

        if inputs.is_empty() {
            bail!("channel has no inputs");
        }

        Ok(Self {
            inputs,
            interpolation: match interpolation {
                GltfInterpolation::CubicSpline => Interpolation::CubicSpline,
                GltfInterpolation::Linear => Interpolation::Linear,
                GltfInterpolation::Step => Interpolation::Step,
            },
            outputs,
            target,
        })
    }

    pub fn inputs(&self) -> &[u32] {
        &self.inputs
    }

    pub fn interpolation(&self) -> Interpolation {
        self.interpolation
    }

    pub fn outputs(&self) -> &Outputs {
        &self.outputs
    }

    /// The target joint/bone.
    pub fn target(&self) -> &str {
        &self.target
    }
}

// This is here because GLTF doesn't provide serialize! TODO: Fix!
/// Specifies an interpolation algorithm.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Deserialize, Serialize)]
pub enum Interpolation {
    /// Linear interpolation.
    ///
    /// The animated values are linearly interpolated between keyframes.
    /// When targeting a rotation, spherical linear interpolation (slerp) should be
    /// used to interpolate quaternions. The number output of elements must equal
    /// the number of input elements.
    Linear = 1,

    /// Step interpolation.
    ///
    /// The animated values remain constant to the output of the first keyframe,
    /// until the next keyframe. The number of output elements must equal the number
    /// of input elements.
    Step,

    /// Cubic spline interpolation.
    ///
    /// The animation's interpolation is computed using a cubic spline with specified
    /// tangents. The number of output elements must equal three times the number of
    /// input elements. For each input element, the output stores three elements, an
    /// in-tangent, a spline vertex, and an out-tangent. There must be at least two
    /// keyframes when using this interpolation
    CubicSpline,
}

#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub enum Outputs {
    Rotations(Vec<Quat>),
    Scales(Vec<Vec3>),
    Translations(Vec<Vec3>),
}

impl Outputs {
    /// Returns `true` if the vector contains no elements.
    pub fn is_empty(&self) -> bool {
        match self {
            Self::Rotations(rotations) => rotations.is_empty(),
            Self::Scales(scales) => scales.is_empty(),
            Self::Translations(translations) => translations.is_empty(),
        }
    }

    /// The count of outputs
    pub fn len(&self) -> usize {
        match self {
            Self::Rotations(rotations) => rotations.len(),
            Self::Scales(scales) => scales.len(),
            Self::Translations(translations) => translations.len(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{Channel, Interpolation, Outputs};

    #[test]
    fn deserialize_rejects_empty_channel_target() {
        let invalid = Channel {
            inputs: vec![0],
            interpolation: Interpolation::Linear,
            outputs: Outputs::Translations(vec![[0.0, 0.0, 0.0]]),
            target: String::new(),
        };
        let mut encoded = Vec::new();
        bincode::serde::encode_into_std_write(invalid, &mut encoded, bincode::config::legacy())
            .unwrap();

        let result =
            bincode::serde::decode_from_slice::<Channel, _>(&encoded, bincode::config::legacy());

        assert!(result.is_err());
    }
}
