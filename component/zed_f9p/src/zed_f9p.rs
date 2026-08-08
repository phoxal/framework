//! `zed_f9p` - u-blox ZED-F9P GNSS component driver.

use anyhow::{Result, anyhow};
use phoxal::prelude::*;

const BACKEND_UNAVAILABLE: &str =
    "zed_f9p hardware backend unavailable: refusing to publish fabricated GNSS measurements";

/// The API remains empty until a real ZED-F9P transport is implemented.
pub(crate) struct Api;

/// Setup never returns state while the hardware backend is unavailable.
pub(crate) struct ZedF9pState;

#[phoxal::driver(state = ZedF9pState, api = Api)]
pub(crate) struct ZedF9p;

fn unavailable_backend() -> anyhow::Error {
    anyhow!(BACKEND_UNAVAILABLE)
}

impl Participant for ZedF9p {
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
    use super::{BACKEND_UNAVAILABLE, ZedF9p};

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn setup_fails_before_publishing_when_hardware_backend_is_unavailable() {
        let (owner, bus) = phoxal_bus::BusOwner::open(phoxal_bus::BusConfig::in_process(
            phoxal_bus::ParticipantId::new("zed-f9p-test").expect("valid participant id"),
        ))
        .await
        .expect("the in-process test bus opens");
        let launch = phoxal::__private::TestHarness::new("zed-f9p-test")
            .expect("valid test participant")
            .with_execution_origin(phoxal::__private::ExecutionOrigin::mint());
        let result = phoxal::__private::run_test_harness::<ZedF9p, _>(&bus, launch, async {}).await;
        owner.close().await.expect("the in-process test bus closes");

        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
