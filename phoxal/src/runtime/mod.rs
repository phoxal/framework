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
//! `decision_key`, `now_ns`, and `suppressed_count`.
//! Decision logging is observability only; it does not create a bus topic or
//! product.

pub mod clock;
pub mod conventions;
pub mod decision_log;
pub mod execute;
pub mod query;
pub mod sensor;

mod args;
mod input;
mod io;
mod process;

use std::borrow::Cow;
use std::time::Duration;

use anyhow::Result;

use crate::runtime::clock::Step;

pub use args::*;
pub use conventions::*;
pub use execute::execute;
pub use input::*;
pub use io::*;
pub use process::*;
pub use query::{QueryOptions, ReadCell, Reader};

#[derive(
    Debug, Copy, Clone, Eq, PartialEq, clap::ValueEnum, serde::Serialize, serde::Deserialize,
)]
#[serde(rename_all = "lowercase")]
pub enum Phase {
    P0,
    P1,
    P2,
    P3,
    P4,
    P5,
}

impl Phase {
    pub const ALL: &'static [Phase] = &[
        Phase::P0,
        Phase::P1,
        Phase::P2,
        Phase::P3,
        Phase::P4,
        Phase::P5,
    ];

    pub const fn slug(self) -> &'static str {
        match self {
            Self::P0 => "p0",
            Self::P1 => "p1",
            Self::P2 => "p2",
            Self::P3 => "p3",
            Self::P4 => "p4",
            Self::P5 => "p5",
        }
    }
}

/// Static descriptor used by `Runtime::scenarios()` and the
/// `scenarios list` subcommand.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "kebab-case")]
pub struct ScenarioDescriptor {
    pub name: Cow<'static, str>,
    pub summary: Cow<'static, str>,

    /// Execution mode: in-process headless or Webots-backed live bus.
    pub kind: ScenarioKind,

    /// Delivery phase this scenario contributes to.
    pub phase: Phase,

    /// Wallclock budget. Orchestrator kills the scenario after this.
    pub timeout_secs: u64,

    /// Logical category for grouping in reports.
    pub category: Cow<'static, str>,

    /// Validation tier (1 = lifted in-process, 2 = Webots integration,
    /// 3 = robot acceptance, etc.).
    pub tier: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "kebab-case")]
pub enum ScenarioKind {
    /// In-process scenario; no live bus. Orchestrator passes --robot-config
    /// and spawns the owning runtime binary.
    Headless,

    /// Live-bus scenario requiring a Webots session running the named world.
    Webots { world: Cow<'static, str> },
}

#[async_trait::async_trait]
pub trait Runtime: Sized + Send {
    /// Stable identifier this runtime reports to the presence/liveness monitor.
    /// Must match the runtime's docker-compose service name (e.g. "map", "follow").
    const RUNTIME_ID: &'static str;

    /// Per-runtime CLI extension. Use `EmptyArgs` when no extra flags are needed.
    type Args: clap::Args + Send + Sync;
    type Config: Send;
    type Input: Send + 'static;

    /// Resolve typed runtime config from runtime-specific and framework-common args.
    fn config(args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config>;

    /// Clock period the runtime's step loop drives at.
    fn clock_period(config: &Self::Config) -> Duration;

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self>;

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()>;

    async fn shutdown(&mut self) -> Result<()> {
        Ok(())
    }

    /// Scenario catalog this runtime owns. Default: empty.
    fn scenarios() -> &'static [ScenarioDescriptor] {
        &[]
    }

    /// Run a scenario by name. Default: bail out with a clear error.
    async fn run_scenario(
        name: &str,
        _common: &RobotRuntimeArgs,
        _args: &Self::Args,
    ) -> Result<()> {
        anyhow::bail!(
            "runtime '{}' has no scenarios registered; cannot run scenario '{}'",
            Self::RUNTIME_ID,
            name
        )
    }
}

#[cfg(test)]
mod tests {
    use crate::runtime::clock::{Schedule, SchedulePolicy};

    #[test]
    fn collapse_schedule_respects_one_hz_cadence() {
        let mut schedule = Schedule::from_publish_hz(1.0, SchedulePolicy::Collapse);

        for tick in 1..10 {
            assert_eq!(schedule.due_steps(tick * 100_000_000), 0);
        }

        assert_eq!(schedule.due_steps(1_000_000_000), 1);
        for tick in 11..20 {
            assert_eq!(schedule.due_steps(tick * 100_000_000), 0);
        }
        assert_eq!(schedule.due_steps(2_000_000_000), 1);
    }

    #[test]
    fn collapse_schedule_emits_once_after_missed_periods() {
        let mut schedule = Schedule::from_publish_hz(1.0, SchedulePolicy::Collapse);

        assert_eq!(schedule.due_steps(3_500_000_000), 1);
        assert_eq!(schedule.due_steps(3_900_000_000), 0);
        assert_eq!(schedule.due_steps(4_000_000_000), 1);
    }
}
