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
        let (owner, bus) = phoxal_bus::BusOwner::open(phoxal_bus::BusConfig::in_process(
            phoxal_bus::ParticipantId::new("bno085-test").expect("valid participant id"),
        ))
        .await
        .expect("the in-process test bus opens");
        let launch = phoxal::__private::ParticipantLaunch::local("bno085-test")
            .with_execution_origin(phoxal::__private::ExecutionOrigin::mint());
        let result = phoxal::__private::run_with_bus::<Bno085, _>(&bus, launch, async {}).await;
        owner.close().await.expect("the in-process test bus closes");

        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }
}
