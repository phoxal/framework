//! The one process-boundary launch contract, from both ends.
//!
//! The crate-private `Launch` parser is the decoder every participant binary
//! compiles; [`LaunchCommand`] is the encoder the launcher writes with. They are
//! one contract read in two directions, and the round-trip test below is what
//! keeps them one.
//!
//! A launched participant receives exactly two facts: who it is and where the
//! supervisor rendezvous is. Clap is the sole parser. There is deliberately no
//! environment fallback, JSON launch envelope, or launch-time copy of a fact
//! the participant learns from the supervisor after attachment.

use std::time::Duration;

use crate::Result;
use crate::identity::ParticipantId;
use clap::Parser;

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
/// The participant learns its model, configuration, and time domain from the
/// supervisor after opening the rendezvous. The bus owner mints its producer
/// identity once the execution is known. No field has an environment fallback.
#[derive(Clone, Debug, Parser)]
#[command(
    name = "phoxal-participant",
    about = "Run one participant from an installed Phoxal bundle.",
    long_about = None
)]
pub(crate) struct Launch {
    /// The identity this process runs under. It is the liveliness key segment,
    /// and it is what the manifest is read by: a service reads
    /// `services.<id>.config`, a driver reads `components.<id>.driver`.
    #[arg(long, value_name = "ID", value_parser = parse_participant_id)]
    pub(crate) participant_id: ParticipantId,

    /// The one rendezvous endpoint for the execution supervisor.
    #[arg(
        long = "connect",
        value_name = "ENDPOINT",
        required = true,
        value_parser = parse_connect_endpoint
    )]
    pub(crate) connect: String,
}

impl Launch {
    /// Parse the process argv without consulting process environment state.
    pub(crate) fn parse() -> Result<Self> {
        Self::try_parse().map_err(anyhow::Error::from)
    }
}

/// The argv one launched participant receives, written by whatever launches it.
///
/// `Launch` above is the decoder, compiled into every participant binary;
/// this is the encoder, and the two are the same contract read from opposite
/// ends. It exists because the launcher is not the supervisor: the CLI starts
/// participants locally and writes the systemd units that start them on a
/// device, so the flag spellings would otherwise live in a second repository
/// and drift from the parser that has to accept them.
///
/// There is no environment half to encode. A launched participant reads two
/// facts from argv and nothing from the environment, deliberately, so this type
/// renders argv and stops.
///
/// ```ignore
/// use phoxal::participant::launch::LaunchCommand;
///
/// let argv = LaunchCommand::for_rendezvous(
///     participant,
///     "unixsock-stream//run/phoxal/supervisor.sock",
/// )
/// .argv();
/// ```
#[allow(
    dead_code,
    reason = "the encoder is the host half of this contract; a participant decodes"
)]
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct LaunchCommand {
    participant_id: ParticipantId,
    connect: String,
}

#[allow(
    dead_code,
    reason = "the encoder is the host half of this contract; a participant decodes"
)]
impl LaunchCommand {
    /// Build the argv for `participant_id` joining one supervisor rendezvous.
    ///
    /// The explicit name is part of the breaking contract: older releases used
    /// a two-argument `new` for `(participant_id, bundle_root)`. Reusing that
    /// shape for `(participant_id, connect)` would let a stale launcher compile
    /// while silently rendering the endpoint as `--bundle-root`.
    #[must_use]
    pub fn for_rendezvous(participant_id: ParticipantId, connect: impl Into<String>) -> Self {
        Self {
            participant_id,
            connect: connect.into(),
        }
    }

    /// Render the argv, without the program name.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        vec![
            "--participant-id".to_owned(),
            self.participant_id.as_str().to_owned(),
            "--connect".to_owned(),
            self.connect.clone(),
        ]
    }
}

fn parse_participant_id(value: &str) -> std::result::Result<ParticipantId, String> {
    value
        .parse()
        .map_err(|error: crate::identity::ParticipantIdError| error.to_string())
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
            "--connect",
            "tcp/router-a:7447",
        ]
    }

    #[test]
    fn accepts_exactly_identity_and_one_rendezvous_endpoint() {
        let launch = Launch::try_parse_from(args()).expect("valid launch argv");
        assert_eq!(launch.participant_id.as_str(), "drive");
        assert_eq!(launch.connect, "tcp/router-a:7447");
    }

    #[test]
    fn refuses_repeated_connect_endpoints() {
        let mut argv = args();
        argv.extend(["--connect", "tcp/router-b:7447"]);
        let error = Launch::try_parse_from(argv).expect_err("one endpoint is the ABI");
        assert_eq!(error.kind(), ErrorKind::ArgumentConflict);
    }

    #[test]
    fn missing_required_fields_fails_before_bundle_or_bus_work() {
        let error = Launch::try_parse_from(["participant-bin"])
            .expect_err("required launch fields must not have defaults");
        assert_eq!(error.kind(), ErrorKind::MissingRequiredArgument);
    }

    #[test]
    fn a_malformed_participant_id_is_rejected() {
        let mut invalid_participant = args();
        invalid_participant[2] = "Drive";
        assert!(Launch::try_parse_from(invalid_participant).is_err());
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
            vec!["--bundle-root", "/var/lib/phoxal/bundle"],
            vec!["--simulation"],
        ] {
            let mut argv = args();
            argv.extend(retired.iter().copied());
            let error =
                Launch::try_parse_from(argv).expect_err("a retired launch flag has no parser");
            assert_eq!(error.kind(), ErrorKind::UnknownArgument, "{retired:?}");
        }
    }

    /// The process boundary is exactly this flag set under exactly these
    /// spellings. A renamed long option or parser alias would create a second
    /// launch contract even if the Rust field remained unchanged.
    #[test]
    fn the_long_flag_set_is_exactly_the_two_launch_facts() {
        let command = Launch::command();
        let mut longs = command
            .get_arguments()
            .filter_map(clap::Arg::get_long)
            .collect::<Vec<_>>();
        longs.sort_unstable();
        assert_eq!(longs, ["connect", "participant-id"]);
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
        argv[4] = "";
        assert!(Launch::try_parse_from(argv).is_err());
    }

    /// The encoder writes what the decoder accepts. This is the whole reason
    /// the encoder is here rather than in whatever repository happens to be
    /// launching a participant this week.
    #[test]
    fn the_encoder_writes_exactly_what_the_decoder_accepts() {
        let command = LaunchCommand::for_rendezvous(
            ParticipantId::new("drive").expect("a valid participant id"),
            "tcp/router-a:7447",
        );

        let argv = command.argv();
        assert_eq!(
            argv,
            [
                "--participant-id",
                "drive",
                "--connect",
                "tcp/router-a:7447",
            ]
        );

        let launch = Launch::try_parse_from(
            std::iter::once("participant-bin".to_owned()).chain(argv.iter().cloned()),
        )
        .expect("the encoder's argv parses");
        assert_eq!(launch.participant_id.as_str(), "drive");
        assert_eq!(launch.connect, "tcp/router-a:7447");
    }

    #[test]
    fn every_process_field_is_clap_only_and_has_no_environment_binding() {
        let command = Launch::command();
        for argument in command.get_arguments() {
            assert!(
                argument.get_env().is_none(),
                "{} unexpectedly reads an environment variable",
                argument.get_id()
            );
        }
    }
}
