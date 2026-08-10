//! `bno085` - BNO085 IMU component driver.

use anyhow::{Result, anyhow};
use phoxal::prelude::*;

const BACKEND_UNAVAILABLE: &str =
    "bno085 hardware backend unavailable: refusing to publish fabricated IMU measurements";

/// The API remains empty until a real BNO085 transport is implemented.
pub(crate) struct Api;

/// Setup never returns state while the hardware backend is unavailable.
pub(crate) struct Bno085State;

#[phoxal::driver(state = Bno085State, api = Api)]
pub(crate) struct Bno085;

fn unavailable_backend() -> anyhow::Error {
    anyhow!(BACKEND_UNAVAILABLE)
}

impl Participant for Bno085 {
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
    use super::{BACKEND_UNAVAILABLE, Bno085};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn setup_fails_before_publishing_when_hardware_backend_is_unavailable() {
        let (owner, bus) = phoxal_bus::BusOwner::open(phoxal_bus::BusConfig::for_participant(
            phoxal_bus::ExecutionId::mint(),
            phoxal_bus::ParticipantId::new("bno085-test").expect("valid participant id"),
            Vec::new(),
        ))
        .await
        .expect("the in-process test bus opens");
        let launch = phoxal::testing::TestHarness::new("bno085-test")
            .expect("valid test participant")
            .with_execution_origin(phoxal::testing::ExecutionOrigin::mint());
        let result =
            phoxal::testing::run_test_harness::<Bno085, _>(&bus, launch, std::future::pending())
                .await;
        owner.close().await;

        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
