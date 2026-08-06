//! Stable data crossing Phoxal process boundaries.
//!
//! This crate deliberately contains no participant runner, bus transport,
//! command-line parser, or project compiler. It is the shared vocabulary used
//! by the framework runtime and `phoxal-cli`: execution identities, the launch
//! record/env ABI, and the strict linker-section metadata record.

mod document;
mod identity;
mod launch;
mod metadata;
mod origin;

pub use document::{COMPONENT_DOCUMENT_SCHEMA, ROBOT_DOCUMENT_SCHEMA, SIMULATION_DOCUMENT_SCHEMA};
pub use identity::{ExecutionId, InvalidIdentity, ProducerId, TimelineId};
pub use launch::{
    BusProfile, ClockMode, DEFAULT_SHUTDOWN_GRACE_MS, LAUNCH_ABI, LaunchEnv, LaunchError,
    ParticipantLaunch, env,
};
pub use metadata::{
    ApiId, MetadataError, PARTICIPANT_METADATA_SCHEMA, ParticipantKind, ParticipantMetadata,
    ParticipantSchemas, SchemaId, parse_participant_metadata,
};
pub use origin::{BootId, ExecutionOrigin};
