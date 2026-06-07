pub mod clock;
pub mod conventions;
pub mod execute;
pub mod presence;
pub mod query;
pub mod sensor_store;
pub mod sim_clock;
pub mod sim_pose;
pub mod sim_reset;
pub mod sim_status;
pub mod staged;
pub mod step;

use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use clap::Parser;
use phoxal_core_structure::Structure;
use phoxal_infra_bus::Bus;
use phoxal_infra_bus::builder::Builder;
use phoxal_infra_helpers::parse_trimmed_non_empty;

pub use conventions::*;
pub use execute::execute;
pub use query::{QueryOptions, ReadCell, Reader};
pub use step::EmptyArgs;

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

    pub fn robot(&self) -> Result<staged::Robot> {
        staged::Robot::read_from_dir(&self.robot_config)
    }

    pub fn resolved_facts(&self) -> Result<phoxal_core_robot::v1::ResolvedFacts> {
        self.robot()?.resolve()
    }

    pub fn structure(&self) -> Result<Structure> {
        let structure = Structure::read_from_dir(&self.robot_config)?;
        structure.validate()?;
        Ok(structure)
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
