//! `ParticipantLaunch` - the clap/env process launch contract.
//!
//! Participant binaries share one common `--flag` set with matching `PHOXAL_*`
//! env fallbacks. Clocked services and drivers additionally accept `--clock` /
//! `PHOXAL_CLOCK`; tools and simulators do not expose either input. Supervisors
//! and systemd units use env, while humans can use flags for bench runs. Flags
//! win over env through clap's native precedence, and `--help` is the
//! user-facing contract documentation.

use std::path::PathBuf;

use clap::{CommandFactory, FromArgMatches};
pub use phoxal_runtime_contract::{
    BusProfile, ClockMode, DEFAULT_SHUTDOWN_GRACE_MS, ExecutionId, ExecutionOrigin, LaunchEnv,
    ParticipantLaunch, ProducerId, env,
};

/// The clap-derived launch fields shared by every participant binary.
#[derive(Debug, clap::Args)]
struct CommonLaunchCli {
    /// Bus-unique participant id. Defaults to the compiled participant artifact id.
    #[arg(
        long,
        env = env::PARTICIPANT_ID,
        hide_env_values = true,
        value_name = "ID"
    )]
    participant_id: Option<String>,

    /// The supervised run to join. Absent means an unmanaged local run, which
    /// mints its own.
    #[arg(
        long,
        env = env::EXECUTION_ID,
        hide_env_values = true,
        value_name = "ID"
    )]
    execution_id: Option<String>,

    /// Supervisor-pre-minted producer identity. Absent means mint one.
    #[arg(
        long,
        env = env::PRODUCER_ID,
        hide_env_values = true,
        value_name = "ID"
    )]
    producer_id: Option<String>,

    /// Supervisor-minted origin of real robot time for this execution.
    #[arg(
        long,
        env = env::EXECUTION_ORIGIN,
        hide_env_values = true,
        value_name = "ORIGIN"
    )]
    execution_origin: Option<String>,

    /// Robot id for the transport root. Defaults to `robot` for local runs.
    #[arg(long, env = env::ROBOT_ID, hide_env_values = true, value_name = "ID")]
    robot_id: Option<String>,

    /// Bus namespace for the transport root.
    #[arg(
        long,
        env = env::NAMESPACE,
        hide_env_values = true,
        value_name = "NAMESPACE",
        default_value = "dev"
    )]
    namespace: Option<String>,

    /// Root directory containing the resolved robot model.
    #[arg(
        long,
        env = env::BUNDLE_ROOT,
        hide_env_values = true,
        value_name = "DIR"
    )]
    bundle_root: Option<PathBuf>,

    /// Component instance id for driver launches.
    #[arg(
        long,
        env = env::COMPONENT_INSTANCE,
        hide_env_values = true,
        value_name = "ID"
    )]
    component_instance: Option<String>,

    /// Comma-separated Zenoh connect endpoints. Empty means in-process.
    #[arg(
        long,
        env = env::CONNECT,
        hide_env_values = true,
        value_name = "ENDPOINTS"
    )]
    connect: Option<String>,

    /// Inline JSON participant config block.
    #[arg(
        long,
        env = env::CONFIG,
        hide_env_values = true,
        value_name = "JSON"
    )]
    config: Option<String>,
}

/// Launch contract for clock-selectable services and drivers.
#[derive(Debug, clap::Parser)]
#[command(
    name = "phoxal-participant",
    about = "Run a Phoxal participant.",
    long_about = None
)]
struct ClockedLaunchCli {
    #[command(flatten)]
    common: CommonLaunchCli,

    /// Clock mode for robot-state execution.
    #[arg(
        long,
        env = env::CLOCK,
        hide_env_values = true,
        value_parser = parse_clock_mode,
        default_value_t = ClockMode::Real
    )]
    clock: ClockMode,
}

/// Launch contract for host/event-driven tools. It intentionally has no clock
/// flag or environment binding.
#[derive(Debug, clap::Parser)]
#[command(
    name = "phoxal-tool",
    about = "Run a Phoxal tool.",
    long_about = None
)]
struct ToolLaunchCli {
    #[command(flatten)]
    common: CommonLaunchCli,
}

/// Launch contract for host/Webots-driven simulators. It intentionally has no
/// clock flag or environment binding: a simulator produces or observes the
/// semantic simulation clock, but never schedules itself from that feed.
#[derive(Debug, clap::Parser)]
#[command(
    name = "phoxal-simulator",
    about = "Run a Phoxal simulator.",
    long_about = None
)]
struct SimulatorLaunchCli {
    #[command(flatten)]
    common: CommonLaunchCli,
}

impl CommonLaunchCli {
    fn into_launch(
        self,
        default_participant_id: &'static str,
        default_robot_id: &'static str,
    ) -> crate::Result<ParticipantLaunch> {
        ParticipantLaunch::decode(LaunchEnv {
            participant_id: self
                .participant_id
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| default_participant_id.to_string()),
            execution_id: self.execution_id,
            producer_id: self.producer_id,
            execution_origin: self.execution_origin,
            robot_id: self
                .robot_id
                .filter(|value| !value.is_empty())
                .unwrap_or_else(|| default_robot_id.to_string()),
            namespace: self.namespace,
            bundle_root: self.bundle_root,
            component_instance: self.component_instance,
            connect: self.connect,
            config: self.config,
            clock: ClockMode::Real,
        })
        .map_err(anyhow::Error::from)
    }
}

fn command_for<C: CommandFactory>(
    default_participant_id: &'static str,
    default_robot_id: &'static str,
) -> clap::Command {
    C::command()
        .mut_arg("participant_id", |arg| {
            arg.default_value(default_participant_id)
        })
        .mut_arg("robot_id", |arg| arg.default_value(default_robot_id))
}

/// Type-level launch contract emitted by the participant macros.
#[doc(hidden)]
pub trait ParticipantLaunchPolicy: Send + Sync + 'static {
    fn from_cli(
        default_participant_id: &'static str,
        default_robot_id: &'static str,
    ) -> crate::Result<ParticipantLaunch>;

    fn clock_mode(launch: &ParticipantLaunch) -> ClockMode;
}

/// Clock-selectable launch policy for services and drivers.
#[doc(hidden)]
pub struct ClockedParticipantLaunch;

impl ParticipantLaunchPolicy for ClockedParticipantLaunch {
    fn from_cli(
        default_participant_id: &'static str,
        default_robot_id: &'static str,
    ) -> crate::Result<ParticipantLaunch> {
        let matches =
            command_for::<ClockedLaunchCli>(default_participant_id, default_robot_id).get_matches();
        let cli = ClockedLaunchCli::from_arg_matches(&matches)?;
        let mut launch = cli
            .common
            .into_launch(default_participant_id, default_robot_id)?;
        launch.clock = cli.clock;
        Ok(launch)
    }

    fn clock_mode(launch: &ParticipantLaunch) -> ClockMode {
        launch.clock
    }
}

/// Clockless launch policy for tools.
#[doc(hidden)]
pub struct ToolParticipantLaunch;

impl ParticipantLaunchPolicy for ToolParticipantLaunch {
    fn from_cli(
        default_participant_id: &'static str,
        default_robot_id: &'static str,
    ) -> crate::Result<ParticipantLaunch> {
        let matches =
            command_for::<ToolLaunchCli>(default_participant_id, default_robot_id).get_matches();
        let cli = ToolLaunchCli::from_arg_matches(&matches)?;
        cli.common
            .into_launch(default_participant_id, default_robot_id)
    }

    fn clock_mode(_launch: &ParticipantLaunch) -> ClockMode {
        ClockMode::Clockless
    }
}

/// Clockless launch policy for host/Webots-driven simulators.
#[doc(hidden)]
pub struct SimulatorParticipantLaunch;

impl ParticipantLaunchPolicy for SimulatorParticipantLaunch {
    fn from_cli(
        default_participant_id: &'static str,
        default_robot_id: &'static str,
    ) -> crate::Result<ParticipantLaunch> {
        let matches = command_for::<SimulatorLaunchCli>(default_participant_id, default_robot_id)
            .get_matches();
        let cli = SimulatorLaunchCli::from_arg_matches(&matches)?;
        cli.common
            .into_launch(default_participant_id, default_robot_id)
    }

    fn clock_mode(_launch: &ParticipantLaunch) -> ClockMode {
        ClockMode::Clockless
    }
}

fn parse_clock_mode(value: &str) -> Result<ClockMode, String> {
    match value {
        "real" => Ok(ClockMode::Real),
        "simulation" => Ok(ClockMode::Simulation),
        "clockless" => Ok(ClockMode::Clockless),
        _ => Err(format!(
            "invalid clock mode '{value}'; expected real or simulation"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::error::ErrorKind;
    use serial_test::serial;

    fn clear_env() {
        // SAFETY: tests touching process env are `#[serial]`, so no other thread
        // reads/writes these vars concurrently.
        for key in env::ALL {
            unsafe { std::env::remove_var(key) };
        }
    }

    fn parse_clocked_from(args: &[&str]) -> crate::Result<ParticipantLaunch> {
        let matches = command_for::<ClockedLaunchCli>("default-id", "robot")
            .try_get_matches_from(args)
            .map_err(anyhow::Error::from)?;
        let cli = ClockedLaunchCli::from_arg_matches(&matches).map_err(anyhow::Error::from)?;
        let mut launch = cli.common.into_launch("default-id", "robot")?;
        launch.clock = cli.clock;
        Ok(launch)
    }

    fn parse_tool_from(args: &[&str]) -> crate::Result<ParticipantLaunch> {
        let matches = command_for::<ToolLaunchCli>("default-id", "robot")
            .try_get_matches_from(args)
            .map_err(anyhow::Error::from)?;
        let cli = ToolLaunchCli::from_arg_matches(&matches).map_err(anyhow::Error::from)?;
        cli.common.into_launch("default-id", "robot")
    }

    fn parse_simulator_from(args: &[&str]) -> crate::Result<ParticipantLaunch> {
        let matches = command_for::<SimulatorLaunchCli>("default-id", "robot")
            .try_get_matches_from(args)
            .map_err(anyhow::Error::from)?;
        let cli = SimulatorLaunchCli::from_arg_matches(&matches).map_err(anyhow::Error::from)?;
        cli.common.into_launch("default-id", "robot")
    }

    #[test]
    #[serial]
    fn cli_with_nothing_set_matches_local_defaults() {
        clear_env();
        let launch = parse_clocked_from(&["participant-bin"]).unwrap();
        assert_eq!(launch.participant_id, "default-id");
        assert_eq!(launch.robot_id, "robot");
        assert_eq!(launch.namespace, "dev");
        assert_eq!(launch.bundle_root, None);
        assert_eq!(launch.config, None);
        assert!(launch.bus.connect_endpoints.is_empty());
        assert_eq!(launch.clock, ClockMode::Real);
    }

    #[test]
    #[serial]
    fn env_overrides_each_launch_field() {
        clear_env();
        let execution = ExecutionId::mint();
        let producer = ProducerId::mint();
        let origin = ExecutionOrigin::mint();
        // SAFETY: serialized test; see clear_env.
        unsafe {
            std::env::set_var(env::PARTICIPANT_ID, "tof-3");
            std::env::set_var(env::EXECUTION_ID, execution.to_string());
            std::env::set_var(env::PRODUCER_ID, producer.to_string());
            std::env::set_var(env::EXECUTION_ORIGIN, origin.encode());
            std::env::set_var(env::ROBOT_ID, "robot-a");
            std::env::set_var(env::NAMESPACE, "lab");
            std::env::set_var(env::BUNDLE_ROOT, "/robot");
            std::env::set_var(env::COMPONENT_INSTANCE, "tof_front");
            std::env::set_var(env::CONNECT, "tcp/127.0.0.1:7447, tcp/127.0.0.1:7448");
            std::env::set_var(env::CONFIG, r#"{"rate_hz":10}"#);
            std::env::set_var(env::CLOCK, "simulation");
        }
        let launch = parse_clocked_from(&["participant-bin"]).unwrap();
        assert_eq!(launch.participant_id, "tof-3");
        assert_eq!(launch.execution, execution);
        assert_eq!(launch.producer, producer);
        assert_eq!(launch.execution_origin, Some(origin));
        assert_eq!(launch.robot_id, "robot-a");
        assert_eq!(launch.namespace, "lab");
        assert_eq!(
            launch.bundle_root.as_deref(),
            Some(std::path::Path::new("/robot"))
        );
        assert_eq!(launch.component_instance.as_deref(), Some("tof_front"));
        assert_eq!(
            launch.bus.connect_endpoints,
            vec![
                "tcp/127.0.0.1:7447".to_string(),
                "tcp/127.0.0.1:7448".to_string()
            ]
        );
        assert_eq!(launch.config, Some(serde_json::json!({"rate_hz": 10})));
        assert_eq!(launch.clock, ClockMode::Simulation);
        clear_env();
    }

    #[test]
    #[serial]
    fn flags_take_precedence_over_env() {
        clear_env();
        let flag_execution = ExecutionId::mint();
        let flag_producer = ProducerId::mint();
        // SAFETY: serialized test; see clear_env.
        unsafe {
            std::env::set_var(env::PARTICIPANT_ID, "env-participant");
            std::env::set_var(env::EXECUTION_ID, ExecutionId::mint().to_string());
            std::env::set_var(env::PRODUCER_ID, ProducerId::mint().to_string());
            std::env::set_var(env::ROBOT_ID, "env-robot");
            std::env::set_var(env::NAMESPACE, "env-ns");
            std::env::set_var(env::BUNDLE_ROOT, "/env-robot");
            std::env::set_var(env::COMPONENT_INSTANCE, "env-component");
            std::env::set_var(env::CONNECT, "tcp/env:7447");
            std::env::set_var(env::CONFIG, r#"{"source":"env"}"#);
            std::env::set_var(env::CLOCK, "simulation");
        }

        let launch = parse_clocked_from(&[
            "participant-bin",
            "--participant-id",
            "flag-participant",
            "--execution-id",
            &flag_execution.to_string(),
            "--producer-id",
            &flag_producer.to_string(),
            "--robot-id",
            "flag-robot",
            "--namespace",
            "flag-ns",
            "--bundle-root",
            "/flag-robot",
            "--component-instance",
            "flag-component",
            "--connect",
            "tcp/flag:7447",
            "--config",
            r#"{"source":"flag"}"#,
            "--clock",
            "real",
        ])
        .unwrap();

        assert_eq!(launch.participant_id, "flag-participant");
        assert_eq!(launch.execution, flag_execution);
        assert_eq!(launch.producer, flag_producer);
        assert_eq!(launch.robot_id, "flag-robot");
        assert_eq!(launch.namespace, "flag-ns");
        assert_eq!(
            launch.bundle_root.as_deref(),
            Some(std::path::Path::new("/flag-robot"))
        );
        assert_eq!(launch.component_instance.as_deref(), Some("flag-component"));
        assert_eq!(launch.bus.connect_endpoints, vec!["tcp/flag:7447"]);
        assert_eq!(launch.config, Some(serde_json::json!({"source": "flag"})));
        assert_eq!(launch.clock, ClockMode::Real);
        clear_env();
    }

    #[test]
    #[serial]
    fn rejects_invalid_config_json_and_clock() {
        clear_env();
        // SAFETY: serialized test; see clear_env.
        unsafe { std::env::set_var(env::CONFIG, "not json") };
        assert!(parse_clocked_from(&["participant-bin"]).is_err());
        unsafe {
            std::env::remove_var(env::CONFIG);
            std::env::set_var(env::CLOCK, "wallclock");
        }
        let err = command_for::<ClockedLaunchCli>("default-id", "robot")
            .try_get_matches_from(["participant-bin"])
            .unwrap_err();
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        clear_env();
    }

    #[test]
    #[serial]
    fn help_lists_contract_env_names_without_values() {
        clear_env();
        // SAFETY: serialized test; see clear_env.
        unsafe { std::env::set_var(env::CONFIG, r#"{"secret":"do-not-print"}"#) };

        let mut help = Vec::new();
        command_for::<ClockedLaunchCli>("default-id", "robot")
            .write_long_help(&mut help)
            .unwrap();
        let help = String::from_utf8(help).unwrap();

        for (flag, env_name) in [
            ("--participant-id", env::PARTICIPANT_ID),
            ("--execution-id", env::EXECUTION_ID),
            ("--producer-id", env::PRODUCER_ID),
            ("--execution-origin", env::EXECUTION_ORIGIN),
            ("--robot-id", env::ROBOT_ID),
            ("--namespace", env::NAMESPACE),
            ("--bundle-root", env::BUNDLE_ROOT),
            ("--component-instance", env::COMPONENT_INSTANCE),
            ("--connect", env::CONNECT),
            ("--config", env::CONFIG),
            ("--clock", env::CLOCK),
        ] {
            assert!(help.contains(flag), "help should list {flag}");
            assert!(help.contains(env_name), "help should list {env_name}");
        }
        assert!(!help.contains("do-not-print"));
        clear_env();
    }

    #[test]
    #[serial]
    fn a_malformed_identity_or_origin_is_rejected_rather_than_silently_replaced() {
        clear_env();

        let error = parse_tool_from(&["tool-bin", "--execution-id", "not-an-id"]).unwrap_err();
        assert!(
            error.to_string().contains("PHOXAL_EXECUTION_ID is invalid"),
            "{error:#}"
        );

        let error = parse_tool_from(&["tool-bin", "--producer-id", "0011"]).unwrap_err();
        assert!(
            error.to_string().contains("PHOXAL_PRODUCER_ID is invalid"),
            "{error:#}"
        );

        let error =
            parse_clocked_from(&["participant-bin", "--execution-origin", "1:2"]).unwrap_err();
        assert!(
            error
                .to_string()
                .contains("PHOXAL_EXECUTION_ORIGIN is malformed"),
            "{error:#}"
        );

        // An unmanaged local run has no supervisor to mint identities, so it
        // mints its own rather than defaulting to a shared constant that two
        // processes would collide on - but it does NOT invent an execution
        // origin, because that would silently defeat the missing-origin
        // trigger instead of reporting an untrustworthy clock.
        let first = parse_tool_from(&["tool-bin"]).unwrap();
        let second = parse_tool_from(&["tool-bin"]).unwrap();
        assert_ne!(first.execution, second.execution);
        assert_ne!(first.producer, second.producer);
        assert_eq!(first.execution_origin, None);
        clear_env();
    }

    #[test]
    #[serial]
    fn a_launch_record_round_trips_its_identities_and_origin_through_json() {
        let launch = ParticipantLaunch::local("drive", "robot")
            .with_execution_origin(ExecutionOrigin::mint());
        let encoded = serde_json::to_string(&launch).unwrap();
        let decoded: ParticipantLaunch = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, launch);
        assert!(encoded.contains(&launch.execution.to_string()));

        let malformed =
            encoded.replace(&launch.execution_origin.unwrap().encode(), "not-an-origin");
        assert!(serde_json::from_str::<ParticipantLaunch>(&malformed).is_err());
    }

    #[test]
    #[serial]
    fn tool_cli_has_no_clock_input() {
        clear_env();
        // A generic supervisor setting is invisible to the tool launch parser.
        // SAFETY: serialized test; see clear_env.
        unsafe { std::env::set_var(env::CLOCK, "simulation") };
        let launch = parse_tool_from(&["tool-bin"]).unwrap();
        assert_eq!(launch.clock, ClockMode::Real);

        let mut help = Vec::new();
        command_for::<ToolLaunchCli>("default-id", "robot")
            .write_long_help(&mut help)
            .unwrap();
        let help = String::from_utf8(help).unwrap();
        assert!(!help.contains("--clock"));
        assert!(!help.contains(env::CLOCK));

        for arguments in [
            vec!["tool-bin", "--clock", "simulation"],
            vec!["tool-bin", "--simulation"],
        ] {
            let error = command_for::<ToolLaunchCli>("default-id", "robot")
                .try_get_matches_from(arguments)
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::UnknownArgument);
        }

        let mut programmatic = ParticipantLaunch::local("tool", "robot");
        programmatic.clock = ClockMode::Simulation;
        assert_eq!(
            ToolParticipantLaunch::clock_mode(&programmatic),
            ClockMode::Clockless,
            "a tool ignores a requested clock mode: it joins the execution, not the clock"
        );
        assert_eq!(
            ClockedParticipantLaunch::clock_mode(&programmatic),
            ClockMode::Simulation
        );
        clear_env();
    }

    #[test]
    #[serial]
    fn simulator_cli_has_no_clock_input() {
        clear_env();
        // A generic orchestrator setting is invisible to the simulator launch
        // parser. Simulators are always host/Webots driven.
        // SAFETY: serialized test; see clear_env.
        unsafe { std::env::set_var(env::CLOCK, "simulation") };
        let launch = parse_simulator_from(&["simulator-bin"]).unwrap();
        assert_eq!(launch.clock, ClockMode::Real);

        let mut help = Vec::new();
        command_for::<SimulatorLaunchCli>("default-id", "robot")
            .write_long_help(&mut help)
            .unwrap();
        let help = String::from_utf8(help).unwrap();
        assert!(!help.contains("--clock"));
        assert!(!help.contains(env::CLOCK));

        for arguments in [
            vec!["simulator-bin", "--clock", "simulation"],
            vec!["simulator-bin", "--simulation"],
        ] {
            let error = command_for::<SimulatorLaunchCli>("default-id", "robot")
                .try_get_matches_from(arguments)
                .unwrap_err();
            assert_eq!(error.kind(), ErrorKind::UnknownArgument);
        }

        let mut programmatic = ParticipantLaunch::local("simulator", "robot");
        programmatic.clock = ClockMode::Simulation;
        assert_eq!(
            SimulatorParticipantLaunch::clock_mode(&programmatic),
            ClockMode::Clockless
        );
        assert_eq!(
            ClockedParticipantLaunch::clock_mode(&programmatic),
            ClockMode::Simulation
        );
        clear_env();
    }
}
