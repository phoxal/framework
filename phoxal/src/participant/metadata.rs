//! Participant metadata is part of the process ABI, not runner implementation.

pub use phoxal_runtime_contract::{
    ApiId, MetadataError, PARTICIPANT_METADATA_SCHEMA, ParticipantKind,
    ParticipantMetadata as ParticipantMeta, ParticipantSchemas, SchemaId,
    parse_participant_metadata,
};
