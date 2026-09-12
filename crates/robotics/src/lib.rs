//! Shared physical and robotics vocabulary for Phoxal contracts.
//!
//! This crate contains generated Protobuf messages and their domain validation.
//! It starts no runtime, transport, hardware driver, or simulator.

use std::path::PathBuf;

include!(concat!(env!("OUT_DIR"), "/phoxal.robotics.v1.rs"));

/// Returns the packaged Protobuf include root for contract owners importing
/// shared robotics messages during their build.
#[must_use]
pub fn proto_include_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("proto")
}

/// The original descriptor closure for independent language generation and inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

/// A shared robotics value violates its public domain contract.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ValidationError {
    /// An encoder quantity is present but is not finite.
    #[error("encoder {field} must be finite when present")]
    NonFiniteEncoderValue {
        /// The invalid field name.
        field: &'static str,
    },
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
