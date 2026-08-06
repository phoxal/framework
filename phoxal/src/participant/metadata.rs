//! Participant metadata is part of the process ABI, not runner implementation.

pub use phoxal_runtime_contract::emit::ParticipantMetadataRecord;
pub use phoxal_runtime_contract::{
    BusAbi, ComponentSchema, LaunchAbi, MetadataError, ParticipantKind,
    ParticipantMetadata as ParticipantMeta, ParticipantSchemas, RobotApi, RobotSchema,
    SimulationSchema, parse_participant_metadata,
};
