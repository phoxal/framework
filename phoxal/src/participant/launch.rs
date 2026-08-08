//! The one process-boundary launch contract.
//!
//! A supervised participant receives only facts owned by the execution
//! supervisor: the execution identity, the compiled participant identity, the
//! immutable runtime bundle, router endpoints, the optional real-time origin,
//! and the teardown grace.  Clap is the sole parser.  There is deliberately no
//! environment fallback, JSON launch envelope, local execution mint, or
//! launch-time copy of facts already authoritative in `runtime.json`.

use std::path::PathBuf;

use crate::Result;
use clap::Parser;
#[cfg(feature = "test-harness")]
use phoxal_runtime_contract::identity::ParticipantIdError;
use phoxal_runtime_contract::identity::{ExecutionId, ParticipantId};
use phoxal_runtime_contract::origin::ExecutionOrigin;

/// Default bounded shutdown grace, in milliseconds.
pub const DEFAULT_SHUTDOWN_GRACE_MS: u64 = 2000;

/// The strict process-boundary contract for one supervised participant.
///
/// Every field is required except the execution origin and the shutdown grace
/// default.  Robot identity, participant configuration, component binding,
/// and scheduler policy are read from the selected compiled runtime record;
/// the participant bus owner mints its own producer identity after that record
/// has been validated.  No field has an environment fallback.
#[derive(Clone, Debug, Parser)]
#[command(
    name = "phoxal-participant",
    about = "Run one participant from a validated Phoxal runtime bundle.",
    long_about = None
)]
pub(crate) struct SupervisedLaunch {
    /// The supervisor-owned execution identity and router key root.
    #[arg(long, value_name = "ID", value_parser = parse_execution_id)]
    pub(crate) execution_id: ExecutionId,

    /// The exact participant record to select from runtime.json.
    #[arg(long, value_name = "ID", value_parser = parse_participant_id)]
    pub(crate) participant_id: ParticipantId,

    /// The installed runtime bundle containing runtime.json, assets/, and bin/.
    #[arg(long, value_name = "DIR")]
    pub(crate) bundle_root: PathBuf,

    /// A router endpoint. Repeat --connect once for each endpoint.
    #[arg(
        long = "connect",
        value_name = "ENDPOINT",
        required = true,
        value_parser = parse_connect_endpoint
    )]
    pub(crate) connect_endpoints: Vec<String>,

    /// The supervisor-minted origin for real robot time.
    #[arg(long, value_name = "ORIGIN", value_parser = parse_execution_origin)]
    pub(crate) execution_origin: Option<ExecutionOrigin>,

    /// Maximum time granted to participant shutdown and owned cleanup.
    #[arg(
        long,
        value_name = "MILLISECONDS",
        default_value_t = DEFAULT_SHUTDOWN_GRACE_MS,
        value_parser = clap::value_parser!(u64).range(1..)
    )]
    pub(crate) shutdown_grace_ms: u64,
}

/// Explicit in-process test input.  This type is not a process launch
/// protocol: it has no bundle/config/clock override and cannot open a bus.
/// Tests supply it only alongside a caller-owned `BusHandle`.
#[cfg(feature = "test-harness")]
#[doc(hidden)]
#[derive(Clone, Debug)]
pub struct TestHarness {
    pub(crate) participant_id: ParticipantId,
    pub(crate) execution_origin: ExecutionOrigin,
    pub(crate) shutdown_grace_ms: u64,
}

#[cfg(feature = "test-harness")]
impl TestHarness {
    /// Construct an explicit test-harness input for a participant id.
    pub fn new(participant_id: impl Into<String>) -> std::result::Result<Self, ParticipantIdError> {
        Ok(Self {
            participant_id: ParticipantId::new(participant_id)?,
            execution_origin: ExecutionOrigin::mint(),
            shutdown_grace_ms: DEFAULT_SHUTDOWN_GRACE_MS,
        })
    }

    /// Use a caller-selected execution origin in a test clock domain.
    #[must_use]
    pub fn with_execution_origin(mut self, origin: ExecutionOrigin) -> Self {
        self.execution_origin = origin;
        self
    }
}

impl SupervisedLaunch {
    /// Parse the process argv without consulting process environment state.
    pub(crate) fn parse() -> Result<Self> {
        Self::try_parse().map_err(anyhow::Error::from)
    }
}

fn parse_execution_id(value: &str) -> std::result::Result<ExecutionId, String> {
    ExecutionId::parse(value).map_err(|error| error.to_string())
}

fn parse_participant_id(value: &str) -> std::result::Result<ParticipantId, String> {
    value
        .parse()
        .map_err(|error: phoxal_runtime_contract::identity::ParticipantIdError| error.to_string())
}

fn parse_execution_origin(value: &str) -> std::result::Result<ExecutionOrigin, String> {
    ExecutionOrigin::decode(value)
        .ok_or_else(|| "execution origin must be <boot>:<boot-ns>:<nonzero-timeline>".to_string())
}

fn parse_connect_endpoint(value: &str) -> std::result::Result<String, String> {
    if value.is_empty()
        || value.trim() != value
        || value.bytes().any(|byte| byte.is_ascii_control())
    {
        return Err("connect endpoint must be non-empty and contain no surrounding whitespace or control characters".to_string());
    }
    Ok(value.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::{CommandFactory, error::ErrorKind};

    const EXECUTION: &str = "10000000000000000000000000000001";
    const ORIGIN: &str = "7:42:9";

    fn args() -> Vec<&'static str> {
        vec![
            "participant-bin",
            "--execution-id",
            EXECUTION,
            "--participant-id",
            "drive",
            "--bundle-root",
            "/var/lib/phoxal/bundle",
            "--connect",
            "tcp/router-a:7447",
        ]
    }

    #[test]
    fn accepts_only_the_supervisor_owned_fields() {
        let launch = SupervisedLaunch::try_parse_from(args()).expect("valid supervised argv");
        assert_eq!(launch.execution_id.to_string(), EXECUTION);
        assert_eq!(launch.participant_id.as_str(), "drive");
        assert_eq!(launch.bundle_root, PathBuf::from("/var/lib/phoxal/bundle"));
        assert_eq!(launch.connect_endpoints, ["tcp/router-a:7447"]);
        assert_eq!(launch.execution_origin, None);
        assert_eq!(launch.shutdown_grace_ms, DEFAULT_SHUTDOWN_GRACE_MS);
    }

    #[test]
    fn accepts_multiple_connect_endpoints_without_a_comma_encoding() {
        let mut argv = args();
        argv.extend([
            "--connect",
            "tcp/router-b:7447",
            "--execution-origin",
            ORIGIN,
        ]);
        let launch = SupervisedLaunch::try_parse_from(argv).expect("valid repeated endpoints");
        assert_eq!(
            launch.connect_endpoints,
            ["tcp/router-a:7447", "tcp/router-b:7447"]
        );
        assert_eq!(
            launch.execution_origin,
            Some(ExecutionOrigin::decode(ORIGIN).expect("test origin"))
        );
    }

    #[test]
    fn missing_required_fields_fails_before_runtime_or_bus_work() {
        let error = SupervisedLaunch::try_parse_from(["participant-bin"])
            .expect_err("required supervised fields must not have defaults");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn malformed_identity_and_origin_are_rejected() {
        let mut invalid_execution = args();
        invalid_execution[2] = "short";
        assert!(SupervisedLaunch::try_parse_from(invalid_execution).is_err());

        let mut invalid_participant = args();
        invalid_participant[4] = "Drive";
        assert!(SupervisedLaunch::try_parse_from(invalid_participant).is_err());

        let mut invalid_origin = args();
        invalid_origin.extend(["--execution-origin", "not-an-origin"]);
        assert!(SupervisedLaunch::try_parse_from(invalid_origin).is_err());
    }

    #[test]
    fn rejected_legacy_launch_fields_have_no_parser_aliases() {
        for field in [
            "--robot",
            "--robot-id",
            "--namespace",
            "--producer-id",
            "--config",
            "--clock",
            "--component-instance",
        ] {
            let mut argv = args();
            argv.extend([field, "value"]);
            let error = SupervisedLaunch::try_parse_from(argv)
                .expect_err("legacy launch fields must be unknown");
            assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{field}");
        }
    }

    #[test]
    fn shutdown_grace_must_be_nonzero() {
        let mut argv = args();
        argv.extend(["--shutdown-grace-ms", "0"]);
        assert!(SupervisedLaunch::try_parse_from(argv).is_err());
    }

    #[test]
    fn empty_connect_endpoint_is_rejected() {
        let mut argv = args();
        argv[8] = "";
        assert!(SupervisedLaunch::try_parse_from(argv).is_err());
    }

    #[test]
    fn every_process_field_is_clap_only_and_has_no_environment_binding() {
        let command = SupervisedLaunch::command();
        for argument in command.get_arguments() {
            assert!(
                argument.get_env().is_none(),
                "{} unexpectedly reads an environment variable",
                argument.get_id()
            );
        }
    }
}
