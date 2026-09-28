//! The standard encoder vocabulary.
//!
//! Producer-independent encoder observation identity under
//! `phoxal.robotics.v1`: encoder-bearing drivers and the native simulator
//! publish this exact shape for declared `encoder` capabilities.

/// One encoder observation: position and velocity are individually
/// optional because a encoder can report either alone.
#[phoxal::message(package = "phoxal.robotics.v1")]
pub struct EncoderSample {
    #[phoxal(tag = 1)]
    pub position_rad: Option<f64>,
    #[phoxal(tag = 2)]
    pub velocity_radps: Option<f64>,
}

impl EncoderSample {
    /// Validates the optional measured quantities without treating zero
    /// as absent.
    pub fn validate(&self) -> Result<(), super::super::robotics::ValidationError> {
        super::super::robotics::validate_optional_finite(self.position_rad, "position_rad")?;
        super::super::robotics::validate_optional_finite(self.velocity_radps, "velocity_radps")
    }
}
