//! Shared physical and robotics vocabulary for Phoxal contracts.
//!
//! Contains generated Protobuf messages and their domain validation.
//! It starts no runtime, transport, hardware driver, or simulator.

include!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/generated/robotics/phoxal.robotics.v1.rs"
));

/// The original descriptor closure for independent language generation and inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] = include_bytes!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/generated/robotics/phoxal-descriptors.bin"
));

/// A shared robotics value violates its public domain contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// Range bounds or a reported valid distance are incoherent.
    #[error("range distance and limits must be finite, nonnegative, and coherent")]
    InvalidRange,
    /// An encoder quantity is present but is not finite.
    #[error("encoder {field} must be finite when present")]
    NonFiniteEncoderValue {
        /// The invalid field name.
        field: &'static str,
    },
}

impl RangeSample {
    /// Validate metric limits and require a valid return to lie within them.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if !self.min_range_m.is_finite()
            || !self.max_range_m.is_finite()
            || !self.distance_m.is_finite()
            || self.min_range_m < 0.0
            || self.max_range_m <= self.min_range_m
            || self.distance_m < 0.0
            || (self.valid && !(self.min_range_m..=self.max_range_m).contains(&self.distance_m))
        {
            return Err(ValidationError::InvalidRange);
        }
        Ok(())
    }
}

impl EncoderSample {
    /// Validates the optional measured quantities without treating zero as absent.
    pub fn validate(&self) -> Result<(), ValidationError> {
        validate_optional_finite(self.position_rad, "position_rad")?;
        validate_optional_finite(self.velocity_radps, "velocity_radps")
    }
}

fn validate_optional_finite(
    value: Option<f64>,
    field: &'static str,
) -> Result<(), ValidationError> {
    if value.is_some_and(|value| !value.is_finite()) {
        return Err(ValidationError::NonFiniteEncoderValue { field });
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use prost::Name;

    use super::{EncoderSample, FILE_DESCRIPTOR_SET, ValidationError};

    #[test]
    fn range_validity_distinguishes_no_return_from_an_out_of_range_measurement() {
        let mut range = super::RangeSample {
            distance_m: 1.0,
            min_range_m: 0.1,
            max_range_m: 4.0,
            valid: true,
        };
        assert!(range.validate().is_ok());
        range.distance_m = 5.0;
        assert!(range.validate().is_err());
        range.distance_m = 0.0;
        range.valid = false;
        assert!(range.validate().is_ok());
        range.max_range_m = f64::NAN;
        assert!(range.validate().is_err());
    }

    #[test]
    fn optional_encoder_quantities_preserve_absence_and_zero() {
        EncoderSample::default()
            .validate()
            .expect("absence is valid");
        EncoderSample {
            position_rad: Some(0.0),
            velocity_radps: Some(-0.0),
        }
        .validate()
        .expect("zero measurements are valid");
    }

    #[test]
    fn rejects_nonfinite_encoder_quantities() {
        assert_eq!(
            EncoderSample {
                position_rad: Some(f64::NAN),
                velocity_radps: None,
            }
            .validate(),
            Err(ValidationError::NonFiniteEncoderValue {
                field: "position_rad"
            })
        );
        assert_eq!(
            EncoderSample {
                position_rad: None,
                velocity_radps: Some(f64::INFINITY),
            }
            .validate(),
            Err(ValidationError::NonFiniteEncoderValue {
                field: "velocity_radps"
            })
        );
    }

    #[test]
    fn retains_public_protobuf_identity_and_descriptors() {
        assert_eq!(EncoderSample::PACKAGE, "phoxal.robotics.v1");
        assert_eq!(EncoderSample::NAME, "EncoderSample");
        assert!(!FILE_DESCRIPTOR_SET.is_empty());
    }
}
