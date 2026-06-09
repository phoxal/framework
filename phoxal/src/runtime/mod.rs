//! Runtime execution and observability helpers.
//!
//! Runtime decisions are logged through [`decision_log::DecisionLog`], not
//! runtime-local `last_logged_state` fields or free-text event names. The
//! runtime owns the typed decision key, normally derived from its owner-local
//! API `State` contract. `phoxal::runtime` owns the logging mechanics.
//!
//! Each runtime calls `observe(now_ns, key)` once per step with logical time
//! from [`clock::Step`]. The initial key always emits. Identical keys are
//! silent. Changes are emitted only when the key differs from the last emitted
//! key, and the helper bounds flapping with a logical-time `min_interval_ns`;
//! in-window transitions are folded into the next emitted event via
//! `suppressed_count`.
//!
//! All decision logs use one structured tracing event on target
//! `phoxal.runtime.decision` with message `runtime decision changed`. Every
//! event carries `runtime_id`, `decision_label`, `schema_name`,
//! `schema_version`, `decision_key`, `now_ns`, and `suppressed_count`.
//! Decision logging is observability only; it does not create a bus topic or
//! product.

pub mod clock;
pub mod conventions;
pub mod decision_log;
pub mod execute;
pub mod query;
pub mod runtime;
pub mod sensor;

use std::path::PathBuf;
use std::time::Duration;

use crate::bus::Bus;
use crate::bus::builder::Builder;
use crate::model::structure::Structure;
use crate::util::parse_trimmed_non_empty;
use anyhow::Result;
use clap::Parser;

pub use conventions::*;
pub use execute::execute;
pub use query::{QueryOptions, ReadCell, Reader};
pub use runtime::EmptyArgs;

pub const ENV_ROBOT_CONFIG: &str = "ROBOT_CONFIG";
pub const ENV_ROBOT_ROUTER_ENDPOINT: &str = "ROBOT_ROUTER_ENDPOINT";
pub const ENV_ROBOT_SIMULATION: &str = "ROBOT_SIMULATION";
pub const ENV_ROBOT_CONNECT_TIMEOUT_MS: &str = "ROBOT_CONNECT_TIMEOUT_MS";
pub const ENV_ROBOT_CONNECT_RETRIES: &str = "ROBOT_CONNECT_RETRIES";
pub const ENV_COMPONENT_ID: &str = "COMPONENT_ID";
pub const ENV_ROBOT_ID: &str = "ROBOT_ID";
pub const ENV_ROBOT_NAMESPACE: &str = "ROBOT_NAMESPACE";

const DEFAULT_STALE_CYCLE_COUNT: f64 = 2.0;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RobotIdentity {
    pub robot_id: String,
    pub robot_namespace: String,
}

impl RobotIdentity {
    pub fn new(robot_id: impl Into<String>, robot_namespace: impl Into<String>) -> Self {
        Self {
            robot_id: robot_id.into(),
            robot_namespace: robot_namespace.into(),
        }
    }

    pub fn host_name(&self) -> String {
        format!("{}-{}", self.robot_namespace, self.robot_id)
    }
}

pub fn stale_timeout_ns(publish_hz: f64) -> u64 {
    ((DEFAULT_STALE_CYCLE_COUNT / publish_hz) * 1_000_000_000.0) as u64
}

/// Shared CLI arguments for all robot binaries.
#[derive(Debug, Parser, Clone)]
pub struct RobotRuntimeArgs {
    /// Path to a bundled robot directory containing robot.yaml, components/, and structure.urdf.
    #[arg(long, env = ENV_ROBOT_CONFIG)]
    pub robot_config: PathBuf,

    #[arg(long, env = ENV_ROBOT_ID, value_parser = parse_trimmed_non_empty)]
    pub robot_id: Option<String>,

    #[arg(
        long,
        env = ENV_ROBOT_NAMESPACE,
        default_value_t = String::from(conventions::DEFAULT_ROBOT_NAMESPACE),
        value_parser = parse_trimmed_non_empty
    )]
    pub robot_namespace: String,

    /// Zenoh router endpoint (for example, tcp/router:7447).
    #[arg(
        long = "robot-router-endpoint",
        env = ENV_ROBOT_ROUTER_ENDPOINT
    )]
    pub robot_router_endpoint: Option<String>,

    /// Consume the shared simulation clock instead of synthesizing a wall clock.
    #[arg(long, env = ENV_ROBOT_SIMULATION, default_value_t = false)]
    pub simulation: bool,

    /// Zenoh connect timeout in milliseconds.
    #[arg(
        long = "robot-connect-timeout-ms",
        env = ENV_ROBOT_CONNECT_TIMEOUT_MS,
        default_value_t = 60_000_u64
    )]
    pub robot_connect_timeout_ms: u64,

    /// Zenoh connection retries after the initial attempt.
    #[arg(
        long = "robot-connect-retries",
        env = ENV_ROBOT_CONNECT_RETRIES,
        default_value_t = 5_u32
    )]
    pub robot_connect_retries: u32,

    /// Hidden process ownership marker used by xtask local session cleanup.
    #[arg(long = "xtask-session", hide = true)]
    pub xtask_session: Option<String>,
}

#[derive(Debug, Parser, Clone)]
pub struct DriverRuntimeArgs {
    #[command(flatten)]
    pub runtime: RobotRuntimeArgs,

    /// Component instance identifier for this component driver service.
    #[arg(long = "component-id", env = ENV_COMPONENT_ID)]
    pub component_id: String,
}

impl RobotRuntimeArgs {
    pub fn identity(&self) -> RobotIdentity {
        RobotIdentity::from(self)
    }

    pub fn connect_timeout(&self) -> Duration {
        Duration::from_millis(self.robot_connect_timeout_ms)
    }

    pub fn robot(&self) -> Result<crate::model::v1::Robot> {
        crate::model::v1::Robot::read_from_dir(&self.robot_config)
    }

    pub fn resolved_facts(&self) -> Result<crate::model::robot::v1::ResolvedFacts> {
        self.robot()?.resolve()
    }

    pub fn structure(&self) -> Result<Structure> {
        Ok(self.robot()?.structure)
    }

    pub async fn connect_bus(&self) -> Result<Bus> {
        Builder::from(self).connect().await.map_err(Into::into)
    }
}

impl DriverRuntimeArgs {
    pub fn identity(&self) -> RobotIdentity {
        self.runtime.identity()
    }

    pub fn simulation(&self) -> bool {
        self.runtime.simulation
    }
}

impl From<&RobotRuntimeArgs> for Builder {
    fn from(args: &RobotRuntimeArgs) -> Self {
        Builder::new(
            args.robot_router_endpoint
                .clone()
                .unwrap_or_else(|| "tcp/router:7447".to_string()),
        )
        .with_connect_timeout(args.connect_timeout())
        .with_connect_retries(args.robot_connect_retries)
        .with_prefix(args.robot_namespace.clone())
    }
}

impl From<RobotRuntimeArgs> for Builder {
    fn from(args: RobotRuntimeArgs) -> Self {
        Self::from(&args)
    }
}

impl From<&RobotRuntimeArgs> for RobotIdentity {
    fn from(args: &RobotRuntimeArgs) -> Self {
        Self::new(
            args.robot_id.clone().unwrap_or_default(),
            args.robot_namespace.clone(),
        )
    }
}

impl From<&DriverRuntimeArgs> for RobotIdentity {
    fn from(args: &DriverRuntimeArgs) -> Self {
        Self::from(&args.runtime)
    }
}
