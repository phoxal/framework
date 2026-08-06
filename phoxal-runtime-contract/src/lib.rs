//! Stable data crossing Phoxal process boundaries.
//!
//! This crate deliberately contains no participant runner, bus transport,
//! command-line parser, or project compiler. It is the shared vocabulary used
//! by the framework runtime and `phoxal-cli`: execution identities, the version
//! identities that cross a process boundary, the launch record/env ABI, and the
//! strict linker-section metadata record.

pub mod emit;
mod identity;
mod launch;
mod metadata;
mod origin;
mod version;

pub use identity::{ExecutionId, InvalidIdentity, ProducerId, TimelineId};
pub use launch::{
    BusProfile, ClockMode, DEFAULT_SHUTDOWN_GRACE_MS, LaunchEnv, LaunchError, ParticipantLaunch,
    env,
};
pub use metadata::{
    MetadataError, ParticipantKind, ParticipantMetadata, ParticipantSchemas,
    parse_participant_metadata,
};
pub use origin::{BootId, ExecutionOrigin};
pub use version::{BusAbi, ComponentSchema, LaunchAbi, RobotApi, RobotSchema, SimulationSchema};
