//! `ParticipantLaunch` - the clap/env process launch contract.
//!
//! Participant binaries share one common `--flag` set with matching `PHOXAL_*`
//! env fallbacks. Clocked services and drivers additionally accept `--clock` /
//! `PHOXAL_CLOCK`; tools and simulators do not expose either input. Supervisors
//! and systemd units use env, while humans can use flags for bench runs. Flags
//! win over env through clap's native precedence, and `--help` is the
//! user-facing contract documentation.

use std::path::PathBuf;

use anyhow::Context;
use clap::{CommandFactory, FromArgMatches};
use serde::{Deserialize, Serialize};

use crate::bus::{ExecutionId, ProducerId};
use crate::participant::clock::ExecutionOrigin;

/// Default bounded shutdown grace, in milliseconds.
pub const DEFAULT_SHUTDOWN_GRACE_MS: u64 = 2000;

/// Public env variable names for the participant launch contract.
pub mod env {
    /// Bus-unique participant id.
    pub const PARTICIPANT_ID: &str = "PHOXAL_PARTICIPANT_ID";
    /// The supervised run this participant joins. Part of the bus key root.
    pub const EXECUTION_ID: &str = "PHOXAL_EXECUTION_ID";
    /// Supervisor-pre-minted producer identity for this process.
    pub const PRODUCER_ID: &str = "PHOXAL_PRODUCER_ID";
    /// Supervisor-minted origin of real robot time for this execution.
    pub const EXECUTION_ORIGIN: &str = "PHOXAL_EXECUTION_ORIGIN";
    /// Robot id for the transport root.
    pub const ROBOT_ID: &str = "PHOXAL_ROBOT_ID";
    /// Bus namespace for the transport root.
    pub const NAMESPACE: &str = "PHOXAL_NAMESPACE";
    /// Root directory containing the resolved robot model.
    pub const ROBOT_ROOT: &str = "PHOXAL_ROBOT_ROOT";
    /// Component instance id for driver launches.
    pub const COMPONENT_INSTANCE: &str = "PHOXAL_COMPONENT_INSTANCE";
    /// Comma-separated Zenoh connect endpoints.
    pub const CONNECT: &str = "PHOXAL_CONNECT";
    /// Inline JSON participant config block.
    pub const CONFIG: &str = "PHOXAL_CONFIG";
    /// Clock mode for services and drivers: real or simulation.
    pub const CLOCK: &str = "PHOXAL_CLOCK";

    /// All env names in contract order.
    pub const ALL: &[&str] = &[
        PARTICIPANT_ID,
        EXECUTION_ID,
        PRODUCER_ID,
        EXECUTION_ORIGIN,
        ROBOT_ID,
        NAMESPACE,
        ROBOT_ROOT,
        COMPONENT_INSTANCE,
        CONNECT,
        CONFIG,
        CLOCK,
    ];
}

/// One participant's launch record.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
pub struct ParticipantLaunch {
    /// The bus-unique participant id (never the static participant/artifact id,
    /// D53). A diagnostic label, never bus identity.
    pub participant_id: String,
    /// The supervised run this participant joins (#952 section B). The
    /// supervisor mints it once per run and every participant carries it; it
    /// scopes the bus key root.
    pub execution: ExecutionId,
    /// This process's producer identity (#952 section G). A supervisor
    /// pre-mints it so restart fencing can re-key on the same value; an
    /// unmanaged local run mints its own.
    pub producer: ProducerId,
    /// The supervisor-minted origin of real robot time. Absent for a run with
    /// no supervisor, in which case a real-clock participant reports itself
    /// unsynchronized rather than inventing an origin.
    #[serde(default, with = "origin_serde")]
    pub execution_origin: Option<ExecutionOrigin>,
    /// The bus namespace (`robot.namespace`).
    pub namespace: String,
    /// The robot id (`robot.id`).
    pub robot_id: String,
    /// The resolved bus profile.
    #[serde(default)]
    pub bus: BusProfile,
    /// The robot clock mode. Tool and simulator launch policies never read this
    /// field; their scheduling is structurally host-driven.
    #[serde(default)]
    pub clock: ClockMode,
    /// The participant's typed config block (`services.<id>.config`), if any.
    #[serde(default)]
    pub config: Option<serde_json::Value>,
    /// The root that holds the robot model (`robot.yaml` + components + structure).
    /// Official participants read it via `SetupContext::robot()`; absent for a bare
    /// local run with no model.
    #[serde(default)]
    pub robot_root: Option<PathBuf>,
    /// The `robot.components` entry this participant drives (D47/D53). A
    /// component driver is launched once per instance, each with a distinct
    /// `participant_id` and its own `component_instance`; read via
    /// `SetupContext::component()`. Absent for non-driver participants.
    #[serde(default)]
    pub component_instance: Option<String>,
    /// Bounded shutdown grace, in milliseconds.
    #[serde(default = "default_grace")]
    pub shutdown_grace_ms: u64,
}

fn default_grace() -> u64 {
    DEFAULT_SHUTDOWN_GRACE_MS
}

impl ParticipantLaunch {
    /// A default launch for local runs: a freshly minted execution and
    /// producer, an in-process bus, and the given participant id (defaulting
    /// the namespace to `dev`, D38).
    ///
    /// The execution **origin** is deliberately absent. Only a supervisor mints
    /// one, and inventing a per-process origin here would defeat the
    /// `TimeUnsynchronized::MissingOrigin` trigger: several participants
    /// launched without `PHOXAL_EXECUTION_ORIGIN` would each silently run on
    /// their own timeline instead of reporting that they have no trustworthy
    /// clock. Use [`with_execution_origin`](Self::with_execution_origin) for a
    /// test or bench run that wants a real one.
    pub fn local(participant_id: impl Into<String>, robot_id: impl Into<String>) -> Self {
        ParticipantLaunch {
            participant_id: participant_id.into(),
            execution: ExecutionId::mint(),
            producer: ProducerId::mint(),
            execution_origin: None,
            namespace: "dev".to_string(),
            robot_id: robot_id.into(),
            bus: BusProfile::default(),
            clock: ClockMode::Real,
            config: None,
            robot_root: None,
            component_instance: None,
            shutdown_grace_ms: DEFAULT_SHUTDOWN_GRACE_MS,
        }
    }

    /// Join an existing supervised run instead of the freshly minted one.
    pub fn in_execution(mut self, execution: ExecutionId) -> Self {
        self.execution = execution;
        self
    }

    /// Anchor real robot time at `origin`. A supervisor mints this once per
    /// run; a bench run that wants a working real clock mints its own.
    pub fn with_execution_origin(mut self, origin: ExecutionOrigin) -> Self {
        self.execution_origin = Some(origin);
        self
    }
}

/// `ExecutionOrigin` rides the JSON launch record as its rendered form, so the
/// record stays a flat, human-readable document rather than exposing the boot
/// identity as three separate fields nobody sets by hand.
mod origin_serde {
    use super::ExecutionOrigin;
    use serde::{Deserialize, Deserializer, Serialize, Serializer};

    pub(super) fn serialize<S: Serializer>(
        value: &Option<ExecutionOrigin>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        value.map(ExecutionOrigin::encode).serialize(serializer)
    }

    pub(super) fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<ExecutionOrigin>, D::Error> {
        let Some(rendered) = Option::<String>::deserialize(deserializer)? else {
            return Ok(None);
        };
        ExecutionOrigin::decode(&rendered).map(Some).ok_or_else(|| {
            serde::de::Error::custom(format!("malformed execution origin '{rendered}'"))
        })
    }
}

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
        env = env::ROBOT_ROOT,
        hide_env_values = true,
        value_name = "DIR"
    )]
    robot_root: Option<PathBuf>,

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
        value_enum,
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
        let participant_id =
            nonempty_or(self.participant_id, || default_participant_id.to_string());
        let robot_id = nonempty_or(self.robot_id, || default_robot_id.to_string());
        let mut launch = ParticipantLaunch::local(participant_id, robot_id);
        if let Some(execution) = self.execution_id.filter(|value| !value.is_empty()) {
            launch.execution = ExecutionId::parse(&execution)
                .map_err(anyhow::Error::msg)
                .context("PHOXAL_EXECUTION_ID is invalid")?;
        }
        if let Some(producer) = self.producer_id.filter(|value| !value.is_empty()) {
            launch.producer = ProducerId::parse(&producer)
                .map_err(anyhow::Error::msg)
                .context("PHOXAL_PRODUCER_ID is invalid")?;
        }
        if let Some(origin) = self.execution_origin.filter(|value| !value.is_empty()) {
            launch.execution_origin = Some(ExecutionOrigin::decode(&origin).ok_or_else(|| {
                anyhow::anyhow!("PHOXAL_EXECUTION_ORIGIN is malformed: '{origin}'")
            })?);
        }
        launch.namespace = nonempty_or(self.namespace, || "dev".to_string());
        launch.robot_root = self.robot_root.filter(|path| !path.as_os_str().is_empty());
        launch.component_instance = self
            .component_instance
            .filter(|instance| !instance.is_empty());
        if let Some(endpoints) = self.connect.filter(|endpoints| !endpoints.is_empty()) {
            launch.bus.connect_endpoints = endpoints
                .split(',')
                .map(|endpoint| endpoint.trim().to_string())
                .filter(|endpoint| !endpoint.is_empty())
                .collect();
        }
        if let Some(config) = self.config.filter(|config| !config.is_empty()) {
            launch.config = Some(
                serde_json::from_str(&config)
                    .context("PHOXAL_CONFIG must be valid JSON for the participant config")?,
            );
        }
        Ok(launch)
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

fn nonempty_or(value: Option<String>, default: impl FnOnce() -> String) -> String {
    value
        .filter(|value| !value.is_empty())
        .unwrap_or_else(default)
}

/// A resolved bus profile: where to connect. Empty endpoints = in-process.
#[derive(Clone, Debug, Default, Serialize, Deserialize, PartialEq, Eq)]
pub struct BusProfile {
    /// Zenoh connect endpoints; empty means in-process (local sim / tests).
    #[serde(default)]
    pub connect_endpoints: Vec<String>,
}

/// The clock mode the runner uses.
///
/// [`ClockMode::Real`] drives scheduled steps from wall time. [`ClockMode::Simulation`]
/// drives the logical-time scheduler from the authoritative
/// `simulation/clock` bus feed.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, clap::ValueEnum)]
#[serde(rename_all = "snake_case")]
pub enum ClockMode {
    /// Host-monotonic real time.
    #[default]
    Real,
    /// Drive scheduled steps from the authoritative `simulation/clock` feed.
    Simulation,
    /// No robot time at all.
    ///
    /// Tools and the externally driven simulation controller join the
    /// *execution*, not the clock (#952 section B): they are given no execution
    /// origin, they have no `Participant::step` to schedule, and they express no robot
    /// time. Selecting `Real` for them would demand an origin their launch
    /// contract deliberately withholds, which is a startup failure rather than
    /// a safety property.
    Clockless,
}

impl std::fmt::Display for ClockMode {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ClockMode::Real => f.write_str("real"),
            ClockMode::Simulation => f.write_str("simulation"),
            ClockMode::Clockless => f.write_str("clockless"),
        }
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
        assert_eq!(launch.robot_root, None);
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
            std::env::set_var(env::ROBOT_ROOT, "/robot");
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
            launch.robot_root.as_deref(),
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
            std::env::set_var(env::ROBOT_ROOT, "/env-robot");
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
            "--robot-root",
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
            launch.robot_root.as_deref(),
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
        assert_eq!(err.kind(), ErrorKind::InvalidValue);
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
            ("--robot-root", env::ROBOT_ROOT),
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
