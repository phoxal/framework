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
        let (owner, bus) = phoxal_bus::BusOwner::open(phoxal_bus::BusConfig::in_process(
            phoxal_bus::ParticipantId::new("ddsm115-test").expect("valid participant id"),
        ))
        .await
        .expect("the in-process test bus opens");
        let launch = phoxal::__private::ParticipantLaunch::local("ddsm115-test")
            .with_execution_origin(phoxal::__private::ExecutionOrigin::mint());
        let result = phoxal::__private::run_with_bus::<Ddsm115, _>(&bus, launch, async {}).await;
        owner.close().await.expect("the in-process test bus closes");

        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
