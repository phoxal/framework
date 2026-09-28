//! The standard range-finder vocabulary.
//!
//! Producer-independent range observation identity under
//! `phoxal.robotics.v1`: range-bearing drivers and the native simulator
//! publish this exact shape for declared `range` capabilities.

/// One range observation: a measured distance with the sensor's coherent
/// limits and validity.
#[phoxal::message(package = "phoxal.robotics.v1")]
pub struct RangeSample {
    #[phoxal(tag = 1)]
    pub distance_m: f64,
    #[phoxal(tag = 2)]
    pub min_range_m: f64,
    #[phoxal(tag = 3)]
    pub max_range_m: f64,
    #[phoxal(tag = 4)]
    pub valid: bool,
}

impl RangeSample {
    /// Validate metric limits and require a valid return to lie within
    /// them.
    pub fn validate(&self) -> Result<(), super::super::robotics::ValidationError> {
        if !self.min_range_m.is_finite()
            || !self.max_range_m.is_finite()
            || !self.distance_m.is_finite()
            || self.min_range_m < 0.0
            || self.max_range_m <= self.min_range_m
            || self.distance_m < 0.0
            || (self.valid && !(self.min_range_m..=self.max_range_m).contains(&self.distance_m))
        {
            return Err(super::super::robotics::ValidationError::InvalidRange);
        }
        Ok(())
    }
}
