//! A local pose fixture for World and Navigation.
//! Ordinary conversions execute in the brain's canonical invocation.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

mod conversions;

phoxal::api!();

use phoxal::contracts::Latest;
use phoxal::runtime::Context;

/// The brain's own expectation of the latest world revision.
#[derive(Clone, Debug)]
pub struct BrainWorldStatus {
    pub revision: u64,
    pub available: bool,
}

/// The brain's empty endpoint contract.
#[phoxal::endpoints]
pub struct BrainApi {
    #[phoxal::input(max_age_ms = 100, max_bytes = 128)]
    world_status: Latest<crate::api::world::WorldRevision>,

    #[phoxal::output(projection = state, bootstrap, max_bytes = 16)]
    heartbeat: Latest<::phoxal::contracts::Empty>,

    #[phoxal::output(projection = state, bootstrap, max_bytes = 512)]
    odometry: Latest<::phoxal::contracts::robotics::OdometryState>,
}

#[derive(Clone, Debug, Default)]
struct Brain {
    odometry: ::phoxal::contracts::robotics::OdometryState,
    world_status: Option<BrainWorldStatus>,
}

#[phoxal::runtime(contract = BrainApi, period_ms = 20)]
impl Brain {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self {
            world_status: None,
            odometry: ::phoxal::contracts::robotics::OdometryState {
                available: true,
                ..Default::default()
            },
        })
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        self.world_status = ctx.world_status().fresh().cloned().map(Into::into);
        self.odometry.revision = self.odometry.revision.saturating_add(1);
        self.odometry.oldest_capture_time_nanos = Some(ctx.now().as_nanos());
        Ok(())
    }

    #[publish(heartbeat)]
    fn heartbeat(&self) -> ::phoxal::contracts::Empty {
        ::phoxal::contracts::Empty::default()
    }

    #[publish(odometry)]
    fn odometry_projection(&self) -> ::phoxal::contracts::robotics::OdometryState {
        self.odometry
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<Brain>()
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::runtime::{ExecutionTime, Harness, ObservationStamp, Sample};
    use std::time::Duration;

    fn sample(revision: u64, nanos: u64) -> Sample<api::world::WorldRevision> {
        Sample::new(
            api::world::WorldRevision {
                revision,
                available: true,
                oldest_capture_time_nanos: Some(nanos),
            },
            ObservationStamp::new("world.revision", ExecutionTime::from_nanos(nanos), Some(19)),
        )
    }

    #[test]
    fn attached_conversion_preserves_provenance_and_expires() -> phoxal::Result<()> {
        let mut brain = Harness::<Brain>::new(())?;
        let input = sample(42, 0);
        let stamp = input.stamp().clone();
        brain.inject_phoxal_conversion_in_0(input)?;
        brain.advance_to(Duration::ZERO)?;
        let output = brain
            .phoxal_conversion_out_0_sample()
            .expect("converted output");
        assert_eq!(output.payload().revision, 42);
        assert_eq!(output.stamp(), &stamp);
        brain.advance_to(Duration::from_millis(120))?;
        assert_eq!(
            brain
                .phoxal_conversion_out_0_sample()
                .expect("retained output")
                .stamp(),
            &stamp
        );
        brain.reset(())?;
        assert!(brain.phoxal_conversion_out_0_sample().is_none());
        brain.advance_to(Duration::from_millis(120))?;
        assert!(brain.phoxal_conversion_out_0_sample().is_none());
        Ok(())
    }

    #[test]
    fn attached_conversion_failure_keeps_the_last_accepted_publication() -> phoxal::Result<()> {
        let mut brain = Harness::<Brain>::new(())?;
        brain.inject_phoxal_conversion_in_0(sample(7, 0))?;
        brain.advance_to(Duration::ZERO)?;
        brain.inject_phoxal_conversion_in_0(sample(u64::MAX, 20_000_000))?;
        let error = brain
            .advance_to(Duration::from_millis(20))
            .expect_err("conversion failure");
        assert!(
            error
                .to_string()
                .contains("navigation.map <- world.revision"),
            "{error}"
        );
        assert!(
            error.to_string().contains("unrepresentable revision"),
            "{error}"
        );
        assert_eq!(
            brain
                .phoxal_conversion_out_0()
                .expect("prior publication")
                .revision,
            7
        );
        assert!(brain.advance_to(Duration::from_millis(40)).is_err());
        brain.reset(())?;
        brain.inject_phoxal_conversion_in_0(sample(8, 40_000_000))?;
        brain.advance_to(Duration::from_millis(40))?;
        assert_eq!(
            brain
                .phoxal_conversion_out_0()
                .expect("new publication")
                .revision,
            8
        );
        Ok(())
    }
}
