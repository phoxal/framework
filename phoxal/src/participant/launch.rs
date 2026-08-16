//! The one process-boundary launch contract.
//!
//! A launched participant receives four facts and nothing else: who it is, the
//! bundle it reads its robot model and its own configuration from, where the
//! router is, and whether this run follows the simulated world clock instead of
//! the host clock (CAMPAIGN.md, "Participant process contract").  Clap is the
//! sole parser.  There is deliberately no environment fallback, JSON launch
//! envelope, or launch-time copy of a fact the participant can learn for
//! itself: the execution identity comes from the router it connects to, and
//! robot time zero is host boot, so neither is an argument.

use std::path::PathBuf;
use std::time::Duration;

use crate::Result;
use clap::Parser;
use phoxal_runtime_contract::identity::ParticipantId;

/// The bounded grace a participant gets for `Participant::shutdown` and owned
/// cleanup.
///
/// A framework-internal constant rather than a launch argument: the launcher
/// has nothing to say about how long this participant's own teardown takes, and
/// a per-process knob only invited two hosts to disagree about it. A launcher
/// that needs a process gone sooner already has SIGKILL.
pub(crate) const SHUTDOWN_GRACE: Duration = Duration::from_millis(2000);

/// The strict process-boundary contract for one launched participant.
///
/// Robot identity, participant configuration, component binding, and scheduler
/// policy are read from `manifest.json` under `--bundle-root`; the participant
/// bus owner mints its own producer identity once the execution is known. No
/// field has an environment fallback.
#[derive(Clone, Debug, Parser)]
#[command(
    name = "phoxal-participant",
    about = "Run one participant from an installed Phoxal bundle.",
    long_about = None
)]
pub(crate) struct SupervisedLaunch {
    /// The identity this process runs under. It is the liveliness key segment,
    /// and it is what the manifest is read by: a service reads
    /// `services.<id>.config`, a driver reads `components.<id>.driver`.
    #[arg(long, value_name = "ID", value_parser = parse_participant_id)]
    pub(crate) participant_id: ParticipantId,

    /// The installed bundle containing manifest.json, assets/, and bin/.
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

    /// Follow the world clock on `runtime/simulation/clock` instead of the host
    /// clock. Simulation is a launch decision, never a bundle fact, so nothing
    /// in the manifest can turn it on.
    #[arg(long = "simulation")]
    pub(crate) simulation: bool,
}

impl SupervisedLaunch {
    /// Parse the process argv without consulting process environment state.
    pub(crate) fn parse() -> Result<Self> {
        Self::try_parse().map_err(anyhow::Error::from)
    }
}

fn parse_participant_id(value: &str) -> std::result::Result<ParticipantId, String> {
    value
        .parse()
        .map_err(|error: phoxal_runtime_contract::identity::ParticipantIdError| error.to_string())
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

    fn args() -> Vec<&'static str> {
        vec![
            "participant-bin",
            "--participant-id",
            "drive",
            "--bundle-root",
            "/var/lib/phoxal/bundle",
            "--connect",
            "tcp/router-a:7447",
        ]
    }

    #[test]
    fn accepts_only_the_four_launch_facts() {
        let launch = SupervisedLaunch::try_parse_from(args()).expect("valid launch argv");
        assert_eq!(launch.participant_id.as_str(), "drive");
        assert_eq!(launch.bundle_root, PathBuf::from("/var/lib/phoxal/bundle"));
        assert_eq!(launch.connect_endpoints, ["tcp/router-a:7447"]);
        assert!(
            !launch.simulation,
            "the host clock is the default; simulation is opted into"
        );
    }

    #[test]
    fn accepts_multiple_connect_endpoints_without_a_comma_encoding() {
        let mut argv = args();
        argv.extend(["--connect", "tcp/router-b:7447", "--simulation"]);
        let launch = SupervisedLaunch::try_parse_from(argv).expect("valid repeated endpoints");
        assert_eq!(
            launch.connect_endpoints,
            ["tcp/router-a:7447", "tcp/router-b:7447"]
        );
        assert!(launch.simulation);
    }

    #[test]
    fn missing_required_fields_fails_before_bundle_or_bus_work() {
        let error = SupervisedLaunch::try_parse_from(["participant-bin"])
            .expect_err("required launch fields must not have defaults");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn a_malformed_participant_id_is_rejected() {
        let mut invalid_participant = args();
        invalid_participant[2] = "Drive";
        assert!(SupervisedLaunch::try_parse_from(invalid_participant).is_err());
    }

    /// Every flag the launch contract retired is now an unknown argument rather
    /// than a quietly accepted one, so a launcher still passing a fact the
    /// participant now learns for itself fails loudly at startup.
    #[test]
    fn the_retired_launch_facts_are_rejected_as_unknown_arguments() {
        for retired in [
            vec!["--execution-id", "10000000000000000000000000000001"],
            vec!["--execution-origin", "7:42:9"],
            vec!["--shutdown-grace-ms", "500"],
        ] {
            let mut argv = args();
            argv.extend(retired.iter().copied());
            let error = SupervisedLaunch::try_parse_from(argv)
                .expect_err("a retired launch flag has no parser");
            assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{retired:?}");
        }
    }

    /// The process boundary is exactly this flag set under exactly these
    /// spellings. A renamed long option or parser alias would create a second
    /// launch contract even if the Rust field remained unchanged.
    #[test]
    fn the_long_flag_set_is_exactly_the_four_launch_facts() {
        let command = SupervisedLaunch::command();
        let mut longs = command
            .get_arguments()
            .filter_map(clap::Arg::get_long)
            .collect::<Vec<_>>();
        longs.sort_unstable();
        assert_eq!(
            longs,
            ["bundle-root", "connect", "participant-id", "simulation"]
        );
        for argument in command.get_arguments() {
            assert!(
                argument
                    .get_all_aliases()
                    .is_none_or(|aliases| aliases.is_empty()),
                "{} declares a parser alias",
                argument.get_id()
            );
            assert!(
                argument.get_short().is_none(),
                "{} declares a short flag; the launch contract is long-only",
                argument.get_id()
            );
            assert!(
                argument
                    .get_all_short_aliases()
                    .is_none_or(|aliases| aliases.is_empty()),
                "{} declares a short parser alias",
                argument.get_id()
            );
        }
    }

    #[test]
    fn empty_connect_endpoint_is_rejected() {
        let mut argv = args();
        argv[6] = "";
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
