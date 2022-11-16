use {
    glam::Quat,
    gltf::animation::Interpolation as GltfInterpolation,
    serde::{Deserialize, Serialize},
};

/// Holds an `Animation` in a `.pak` file. For data transport only.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct AnimationBuf {
    /// The channels (joints/bones) of movement used in this `Animation`.
    pub channels: Vec<Channel>,
}

/// Describes the animation of one joint.
#[derive(Debug, Deserialize, PartialEq, Serialize)]
pub struct Channel {
    inputs: Vec<f32>,
    interpolation: Interpolation,
    rotations: Vec<Quat>,
    target: String,
}

impl Channel {
    #[allow(unused)]
    pub(crate) fn new<T: AsRef<str>, I: IntoIterator<Item = f32>, R: IntoIterator<Item = Quat>>(
        target: T,
        interpolation: GltfInterpolation,
        inputs: I,
        rotations: R,
    ) -> Self {
        let inputs = inputs.into_iter().collect::<Vec<_>>();
        let rotations = rotations.into_iter().collect::<Vec<_>>();
        let target = target.as_ref().to_owned();

        assert!(!target.is_empty());
        assert_ne!(inputs.len(), 0);

        match interpolation {
            GltfInterpolation::Linear | GltfInterpolation::Step => {
                assert_eq!(inputs.len(), rotations.len());
            }
            GltfInterpolation::CubicSpline => {
                assert_eq!(inputs.len() * 3, rotations.len());
            }
        }

        Self {
            inputs,
            interpolation: match interpolation {
                GltfInterpolation::CubicSpline => Interpolation::CubicSpline,
                GltfInterpolation::Linear => Interpolation::Linear,
                GltfInterpolation::Step => Interpolation::Step,
            },
            rotations,
            target,
        }
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
