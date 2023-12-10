use {
    glam::{Quat, Vec3},
    gltf::animation::Interpolation as GltfInterpolation,
    serde::{Deserialize, Serialize},
    std::time::Duration,
};

/// Holds an `Animation` in a `.pak` file. For data transport only.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct AnimationBuf {
    channels: Vec<Channel>,
}

impl AnimationBuf {
    pub(super) fn new(channels: Vec<Channel>) -> Self {
        Self { channels }
    }

    /// The channels (joints/bones) of movement used in this `Animation`.
    pub fn channels(&self) -> &[Channel] {
        &self.channels
    }
}

/// Describes the animation of one joint.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
pub struct Channel {
    inputs: Vec<u32>,
    interpolation: Interpolation,
    outputs: Outputs,
    target: String,
}

impl Channel {
    #[allow(unused)]
    pub(crate) fn new<T: AsRef<str>, I: IntoIterator<Item = u32>>(
        target: T,
        interpolation: GltfInterpolation,
        inputs: I,
        outputs: Outputs,
    ) -> Self {
        let inputs = inputs.into_iter().collect::<Vec<_>>();
        let target = target.as_ref().to_owned();

        assert!(!target.is_empty());
        assert_ne!(inputs.len(), 0);

        Self {
            inputs,
            interpolation: match interpolation {
                GltfInterpolation::CubicSpline => Interpolation::CubicSpline,
                GltfInterpolation::Linear => Interpolation::Linear,
                GltfInterpolation::Step => Interpolation::Step,
            },
            outputs,
            target,
        }
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
    /// The count of outputs
    pub fn len(&self) -> usize {
        match self {
            Self::Rotations(rotations) => rotations.len(),
            Self::Scales(scales) => scales.len(),
            Self::Translations(translations) => translations.len(),
        }
    }
}
