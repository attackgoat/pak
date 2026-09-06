use {
    crate::{MaterialId, MeshId, OpacityMicromapId},
    serde::{Deserialize, Deserializer, Serialize, de::Error as _},
    std::collections::BTreeMap,
};

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub enum OpacityMicromapFormat {
    FourState,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct OpacityMicromapRecipe(pub [u8; 32]);

#[derive(Clone, Copy, Debug, Deserialize, Eq, Hash, Ord, PartialEq, PartialOrd, Serialize)]
pub struct OpacityMicromapKey {
    pub mesh: MeshId,
    pub primitive: u32,
    pub material: MaterialId,
    pub source_mip: u32,
    pub recipe: OpacityMicromapRecipe,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpacityMicromapInfo {
    pub key: OpacityMicromapKey,
    pub payload: OpacityMicromapId,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpacityMicromapDescriptor {
    pub data_offset: u32,
    pub subdivision_level: u16,
    pub format: OpacityMicromapFormat,
}

#[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct OpacityMicromapUsage {
    pub count: u32,
    pub subdivision_level: u16,
    pub format: OpacityMicromapFormat,
}

#[derive(Clone, Debug, Eq, PartialEq, Serialize)]
pub struct OpacityMicromap {
    #[serde(with = "serde_bytes")]
    array_data: Box<[u8]>,
    descriptors: Box<[OpacityMicromapDescriptor]>,
    descriptor_usage: Box<[OpacityMicromapUsage]>,
    indices: Box<[i32]>,
    index_usage: Box<[OpacityMicromapUsage]>,
    triangle_count: u32,
    max_subdivision_level: u16,
}

impl<'de> Deserialize<'de> for OpacityMicromap {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Data {
            #[serde(with = "serde_bytes")]
            array_data: Box<[u8]>,
            descriptors: Box<[OpacityMicromapDescriptor]>,
            descriptor_usage: Box<[OpacityMicromapUsage]>,
            indices: Box<[i32]>,
            index_usage: Box<[OpacityMicromapUsage]>,
            triangle_count: u32,
            max_subdivision_level: u16,
        }

        let data = Data::deserialize(deserializer)?;
        let payload = Self {
            array_data: data.array_data,
            descriptors: data.descriptors,
            descriptor_usage: data.descriptor_usage,
            indices: data.indices,
            index_usage: data.index_usage,
            triangle_count: data.triangle_count,
            max_subdivision_level: data.max_subdivision_level,
        };
        payload.validate().map_err(D::Error::custom)?;
        Ok(payload)
    }
}

impl OpacityMicromap {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        array_data: impl Into<Box<[u8]>>,
        descriptors: impl Into<Box<[OpacityMicromapDescriptor]>>,
        descriptor_usage: impl Into<Box<[OpacityMicromapUsage]>>,
        indices: impl Into<Box<[i32]>>,
        index_usage: impl Into<Box<[OpacityMicromapUsage]>>,
        triangle_count: u32,
        max_subdivision_level: u16,
    ) -> Result<Self, &'static str> {
        let payload = Self {
            array_data: array_data.into(),
            descriptors: descriptors.into(),
            descriptor_usage: descriptor_usage.into(),
            indices: indices.into(),
            index_usage: index_usage.into(),
            triangle_count,
            max_subdivision_level,
        };
        payload.validate()?;
        Ok(payload)
    }

    pub fn array_data(&self) -> &[u8] {
        &self.array_data
    }

    pub fn descriptors(&self) -> &[OpacityMicromapDescriptor] {
        &self.descriptors
    }

    pub fn descriptor_usage(&self) -> &[OpacityMicromapUsage] {
        &self.descriptor_usage
    }

    pub fn indices(&self) -> &[i32] {
        &self.indices
    }

    pub fn index_usage(&self) -> &[OpacityMicromapUsage] {
        &self.index_usage
    }

    pub fn triangle_count(&self) -> u32 {
        self.triangle_count
    }

    pub fn max_subdivision_level(&self) -> u16 {
        self.max_subdivision_level
    }

    pub fn validate(&self) -> Result<(), &'static str> {
        if self.triangle_count == 0 || self.indices.len() != self.triangle_count as usize {
            return Err("opacity micromap index count does not match its triangle count");
        }
        if self.max_subdivision_level > 12 {
            return Err("opacity micromap subdivision level exceeds 12");
        }

        let mut descriptor_histogram = BTreeMap::new();
        let mut represented_max = 0;
        for descriptor in &self.descriptors {
            if descriptor.subdivision_level > self.max_subdivision_level {
                return Err("opacity micromap descriptor subdivision level exceeds its maximum");
            }
            let byte_len = four_state_byte_len(descriptor.subdivision_level)?;
            let end = (descriptor.data_offset as usize)
                .checked_add(byte_len)
                .ok_or("opacity micromap descriptor data range overflows")?;
            if end > self.array_data.len() {
                return Err("opacity micromap descriptor data range is out of bounds");
            }
            increment_histogram(
                &mut descriptor_histogram,
                (descriptor.subdivision_level, descriptor.format),
            )?;
            represented_max = represented_max.max(descriptor.subdivision_level);
        }
        if represented_max != self.max_subdivision_level {
            return Err("opacity micromap maximum subdivision level is inconsistent");
        }
        validate_usage(&self.descriptor_usage, &descriptor_histogram)?;

        let mut index_histogram = BTreeMap::new();
        for &index in &self.indices {
            if index < 0 {
                if !(-4..=-1).contains(&index) {
                    return Err("opacity micromap special index is invalid");
                }
                continue;
            }
            let descriptor = self
                .descriptors
                .get(index as usize)
                .ok_or("opacity micromap descriptor index is out of bounds")?;
            increment_histogram(
                &mut index_histogram,
                (descriptor.subdivision_level, descriptor.format),
            )?;
        }
        validate_usage(&self.index_usage, &index_histogram)?;

        Ok(())
    }
}

fn four_state_byte_len(subdivision_level: u16) -> Result<usize, &'static str> {
    1_usize
        .checked_shl(u32::from(subdivision_level) * 2)
        .map(|microtriangles| microtriangles.div_ceil(4))
        .ok_or("opacity micromap subdivision size overflows")
}

fn increment_histogram(
    histogram: &mut BTreeMap<(u16, OpacityMicromapFormat), u32>,
    key: (u16, OpacityMicromapFormat),
) -> Result<(), &'static str> {
    let count = histogram.entry(key).or_default();
    *count = count
        .checked_add(1)
        .ok_or("opacity micromap usage count overflows")?;
    Ok(())
}

fn validate_usage(
    usage: &[OpacityMicromapUsage],
    expected: &BTreeMap<(u16, OpacityMicromapFormat), u32>,
) -> Result<(), &'static str> {
    let mut actual = BTreeMap::new();
    for usage in usage {
        if usage.count == 0
            || actual
                .insert((usage.subdivision_level, usage.format), usage.count)
                .is_some()
        {
            return Err("opacity micromap usage records are invalid");
        }
    }
    if &actual != expected {
        return Err("opacity micromap usage histogram is inconsistent");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mixed_payload() -> OpacityMicromap {
        OpacityMicromap::new(
            [0, 1],
            [
                OpacityMicromapDescriptor {
                    data_offset: 0,
                    subdivision_level: 0,
                    format: OpacityMicromapFormat::FourState,
                },
                OpacityMicromapDescriptor {
                    data_offset: 1,
                    subdivision_level: 1,
                    format: OpacityMicromapFormat::FourState,
                },
            ],
            [
                OpacityMicromapUsage {
                    count: 1,
                    subdivision_level: 0,
                    format: OpacityMicromapFormat::FourState,
                },
                OpacityMicromapUsage {
                    count: 1,
                    subdivision_level: 1,
                    format: OpacityMicromapFormat::FourState,
                },
            ],
            [0, 1, 1, -1, -4],
            [
                OpacityMicromapUsage {
                    count: 1,
                    subdivision_level: 0,
                    format: OpacityMicromapFormat::FourState,
                },
                OpacityMicromapUsage {
                    count: 2,
                    subdivision_level: 1,
                    format: OpacityMicromapFormat::FourState,
                },
            ],
            5,
            1,
        )
        .unwrap()
    }

    #[test]
    fn payload_round_trip_preserves_signed_special_indices() {
        let expected = mixed_payload();
        let encoded = bincode::serde::encode_to_vec(&expected, bincode::config::legacy()).unwrap();
        let (actual, consumed) = bincode::serde::decode_from_slice::<OpacityMicromap, _>(
            &encoded,
            bincode::config::legacy(),
        )
        .unwrap();

        assert_eq!(consumed, encoded.len());
        assert_eq!(actual, expected);
        assert_eq!(&actual.indices()[3..], &[-1, -4]);
    }

    #[test]
    fn all_special_payload_needs_no_array_or_descriptors() {
        let payload = OpacityMicromap::new([], [], [], [-1, -2, -3, -4], [], 4, 0).unwrap();

        assert!(payload.array_data().is_empty());
        assert!(payload.descriptors().is_empty());
    }

    #[test]
    fn validation_rejects_bad_indices_ranges_and_histograms() {
        let mut payload = mixed_payload();
        payload.indices[0] = 2;
        assert!(payload.validate().is_err());

        let mut payload = mixed_payload();
        payload.indices[0] = -5;
        assert!(payload.validate().is_err());

        let mut payload = mixed_payload();
        payload.descriptors[1].data_offset = 2;
        assert!(payload.validate().is_err());

        let mut payload = mixed_payload();
        payload.index_usage[1].count = 1;
        assert!(payload.validate().is_err());
    }

    #[test]
    fn deserialization_rejects_malformed_payload() {
        let mut payload = mixed_payload();
        payload.indices[0] = -5;
        let encoded = bincode::serde::encode_to_vec(payload, bincode::config::legacy()).unwrap();

        assert!(
            bincode::serde::decode_from_slice::<OpacityMicromap, _>(
                &encoded,
                bincode::config::legacy()
            )
            .is_err()
        );
    }
}
