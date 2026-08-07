//! The publish-rate divisor every sensor driver applies to its step loop.

/// How often a driver publishes one capability's samples, expressed as a
/// divisor of its step cadence.
///
/// A driver steps at a fixed `step_hz` and each capability declares its own
/// `publish_rate_hz`; the driver publishes that capability on every `divisor`th
/// step. Building the schedule once at setup is what makes the per-step check
/// a single integer test.
///
/// # The divisor is rounded
///
/// `divisor` is `(step_hz / publish_rate_hz).round()`, clamped to at least 1,
/// so the achieved rate is `step_hz / divisor` and not, in general, the
/// configured `publish_rate_hz`. A capability configured at 30 Hz under a
/// 100 Hz step loop publishes at `100 / 3 ≈ 33.3` Hz, and any
/// `publish_rate_hz` above `step_hz` publishes every step. The driver reports
/// the configured rate, not the achieved one, so a consumer reading the
/// declared rate is reading a number the driver does not hold to. This type
/// reproduces that behavior exactly rather than quietly changing every
/// driver's cadence; correcting it is a contract change, not a refactor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SampleSchedule {
    divisor: u64,
}

impl SampleSchedule {
    /// Validate `publish_rate_hz` and resolve the step divisor for it.
    ///
    /// `capability_id` only names the offender in the error.
    ///
    /// # Errors
    ///
    /// Returns an error when `publish_rate_hz` is not a positive finite
    /// number, which is the only way a capability can ask for a cadence that
    /// has no divisor at all.
    pub fn new(capability_id: &str, step_hz: f64, publish_rate_hz: f64) -> crate::Result<Self> {
        if !publish_rate_hz.is_finite() || publish_rate_hz <= 0.0 {
            anyhow::bail!("capability '{capability_id}' publish_rate_hz must be > 0");
        }
        Ok(Self {
            divisor: (step_hz / publish_rate_hz).round().max(1.0) as u64,
        })
    }

    /// The number of steps between two publishes; never zero.
    #[must_use]
    pub fn divisor(&self) -> u64 {
        self.divisor
    }

    /// Whether the step numbered `step_index` publishes this capability.
    #[must_use]
    pub fn is_due(&self, step_index: u64) -> bool {
        self.divisor <= 1 || step_index.is_multiple_of(self.divisor)
    }
}

#[cfg(test)]
mod tests {
    use super::SampleSchedule;

    #[test]
    fn a_rate_at_or_above_the_step_cadence_publishes_every_step() {
        for publish_rate_hz in [100.0, 250.0, f64::MAX] {
            let schedule = SampleSchedule::new("imu", 100.0, publish_rate_hz).unwrap();
            assert_eq!(schedule.divisor(), 1);
            assert!((0..5).all(|step| schedule.is_due(step)));
        }
    }

    #[test]
    fn a_slower_rate_publishes_on_every_divisorth_step() {
        let schedule = SampleSchedule::new("range", 100.0, 25.0).unwrap();
        assert_eq!(schedule.divisor(), 4);
        assert_eq!(
            (0..9).map(|step| schedule.is_due(step)).collect::<Vec<_>>(),
            [true, false, false, false, true, false, false, false, true]
        );
    }

    /// The rounding documented on the type: 100/30 rounds to 3, so the driver
    /// publishes at 33.3 Hz while still reporting 30 Hz.
    #[test]
    fn the_divisor_rounds_and_substitutes_a_different_rate() {
        assert_eq!(
            SampleSchedule::new("gnss", 100.0, 30.0).unwrap().divisor(),
            3
        );
        assert_eq!(
            SampleSchedule::new("gnss", 100.0, 40.0).unwrap().divisor(),
            3
        );
    }

    #[test]
    fn a_non_positive_or_non_finite_rate_is_rejected() {
        for publish_rate_hz in [0.0, -1.0, f64::NAN, f64::INFINITY] {
            let error = SampleSchedule::new("camera", 100.0, publish_rate_hz)
                .expect_err("a rate with no divisor must be rejected")
                .to_string();
            assert_eq!(error, "capability 'camera' publish_rate_hz must be > 0");
        }
    }
}
