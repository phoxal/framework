//! `vl53l1x` - VL53L1X range component driver.

use anyhow::{Result, anyhow};
use phoxal::prelude::*;

const BACKEND_UNAVAILABLE: &str =
    "vl53l1x hardware backend unavailable: refusing to publish fabricated range measurements";

/// The API remains empty until a real VL53L1X transport is implemented.
pub(crate) struct Api;

/// Setup never returns state while the hardware backend is unavailable.
pub(crate) struct Vl53l1xState;

#[phoxal::driver(state = Vl53l1xState, api = Api)]
pub(crate) struct Vl53l1x;

fn unavailable_backend() -> anyhow::Error {
    anyhow!(BACKEND_UNAVAILABLE)
}

impl Participant for Vl53l1x {
    async fn setup(
        &self,
        _ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        Err(unavailable_backend())
    }
}

#[cfg(test)]
mod tests {
    use super::{BACKEND_UNAVAILABLE, Vl53l1x};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn setup_fails_before_publishing_when_hardware_backend_is_unavailable() {
        let (owner, bus) = phoxal_bus::BusOwner::open(phoxal_bus::BusConfig::in_process(
            phoxal_bus::ParticipantId::new("vl53l1x-test").expect("valid participant id"),
        ))
        .await
        .expect("the in-process test bus opens");
        let launch = phoxal::__private::TestHarness::new("vl53l1x-test")
            .expect("valid test participant")
            .with_execution_origin(phoxal::__private::ExecutionOrigin::mint());
        let result =
            phoxal::__private::run_test_harness::<Vl53l1x, _>(&bus, launch, std::future::pending())
                .await;
        owner.close().await.expect("the in-process test bus closes");

        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
