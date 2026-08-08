//! `oak_d_lite` - OAK-D Lite camera/depth/IMU component driver.

use anyhow::{Result, anyhow};
use phoxal::prelude::*;

const BACKEND_UNAVAILABLE: &str = "oak_d_lite hardware backend unavailable: refusing to publish fabricated camera, depth, or IMU measurements";

/// The API remains empty until a real OAK-D transport is implemented.
pub(crate) struct Api;

/// Setup never returns state while the hardware backend is unavailable.
pub(crate) struct OakDLiteState;

#[phoxal::driver(state = OakDLiteState, api = Api)]
pub(crate) struct OakDLite;

fn unavailable_backend() -> anyhow::Error {
    anyhow!(BACKEND_UNAVAILABLE)
}

impl Participant for OakDLite {
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
    use super::{BACKEND_UNAVAILABLE, OakDLite};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn setup_fails_before_publishing_when_hardware_backend_is_unavailable() {
        let (owner, bus) = phoxal_bus::BusOwner::open(phoxal_bus::BusConfig::in_process(
            phoxal_bus::ParticipantId::new("oak-d-lite-test").expect("valid participant id"),
        ))
        .await
        .expect("the in-process test bus opens");
        let launch = phoxal::testing::TestHarness::new("oak-d-lite-test")
            .expect("valid test participant")
            .with_execution_origin(phoxal::testing::ExecutionOrigin::mint());
        let result =
            phoxal::testing::run_test_harness::<OakDLite, _>(&bus, launch, std::future::pending())
                .await;
        owner.close().await.expect("the in-process test bus closes");

        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
