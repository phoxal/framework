//! `ddsm115` - Waveshare DDSM115 wheel-motor component driver.

use anyhow::{Result, anyhow};
use phoxal::prelude::*;

const BACKEND_UNAVAILABLE: &str = "ddsm115 hardware backend unavailable: refusing to model motor or publish fabricated encoder measurements";

/// The DDSM115 speaks RS-485, and one bus carries the whole wheel set: every
/// motor on it answers to a distinct id, set on the motor itself. That id is
/// the one thing this driver cannot derive from anything else, so it is the
/// whole of its authored configuration.
#[derive(serde::Deserialize, phoxal::Config)]
#[serde(deny_unknown_fields)]
pub(crate) struct Ddsm115Config {
    /// The motor's own id on the shared RS-485 bus, as configured on the motor.
    // `allow` rather than `expect`: this crate's own tests read the field, so
    // the lint has nothing to say about the test build. It has nothing to read
    // it in the shipped binary only because the RS-485 transport that would
    // address the motor is not implemented yet.
    #[allow(
        dead_code,
        reason = "read once the DDSM115 RS-485 transport exists; it is the authored contract that transport addresses the motor by"
    )]
    pub(crate) id: u8,
}

/// The API remains empty until a real DDSM115 transport is implemented.
pub(crate) struct Api;

/// Setup never returns state while the hardware backend is unavailable.
pub(crate) struct Ddsm115State;

#[phoxal::driver(
    state = Ddsm115State,
    api = Api,
    config = Ddsm115Config,
    connection = serial
)]
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
        let bus = phoxal::testing::TestBus::for_participant("ddsm115-test")
            .await
            .expect("the in-process test bus opens");
        let launch = phoxal::testing::TestHarness::new("ddsm115-test")
            .expect("valid test participant")
            .with_config(serde_json::json!({ "id": 1 }));
        let result = phoxal::testing::run_test_harness::<Ddsm115, _>(
            bus.handle(),
            launch,
            std::future::pending(),
        )
        .await;
        bus.close().await;

        let error = result.expect_err("setup must reject an unavailable hardware backend");
        assert_eq!(error.to_string(), BACKEND_UNAVAILABLE);
    }

    /// The driver's own half of the authored `driver:` block. The motor id is
    /// this crate's whole configuration, and the connection it is reached over
    /// is the framework's half - which is why a bus number cannot be smuggled
    /// in here.
    #[test]
    fn the_driver_config_is_the_motor_id_and_nothing_else() {
        let config: super::Ddsm115Config =
            serde_json::from_value(serde_json::json!({ "id": 3 })).expect("the motor id parses");
        assert_eq!(config.id, 3);
        assert!(
            serde_json::from_value::<super::Ddsm115Config>(
                serde_json::json!({ "id": 3, "bus": 0 })
            )
            .is_err(),
            "an undeclared key in the driver's own config must not parse"
        );
    }
}
