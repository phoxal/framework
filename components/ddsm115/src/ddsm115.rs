//! `ddsm115` - Waveshare DDSM115 wheel-motor component driver.

use anyhow::{Result, anyhow};
use phoxal::prelude::*;

const BACKEND_UNAVAILABLE: &str = "ddsm115 hardware backend unavailable: refusing to model motor or publish fabricated encoder measurements";

/// The API remains empty until a real DDSM115 transport is implemented.
pub(crate) struct Api;

/// Setup never returns state while the hardware backend is unavailable.
pub(crate) struct Ddsm115State;

#[phoxal::driver(state = Ddsm115State, api = Api)]
pub(crate) struct Ddsm115;

fn unavailable_backend() -> anyhow::Error {
    anyhow!(BACKEND_UNAVAILABLE)
}

impl Participant for Ddsm115 {
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
    use super::{BACKEND_UNAVAILABLE, Ddsm115};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn setup_fails_before_modeling_or_publishing_without_hardware() {
        let (owner, bus) = phoxal_bus::BusOwner::open(phoxal_bus::BusConfig::for_participant(
            phoxal_bus::ExecutionId::mint(),
            phoxal_bus::ParticipantId::new("ddsm115-test").expect("valid participant id"),
            Vec::new(),
        ))
        .await
        .expect("the in-process test bus opens");
        let launch = phoxal::testing::TestHarness::new("ddsm115-test")
            .expect("valid test participant")
            .with_execution_origin(phoxal::testing::ExecutionOrigin::mint());
        let result =
            phoxal::testing::run_test_harness::<Ddsm115, _>(&bus, launch, std::future::pending())
                .await;
        owner.close().await;

        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
