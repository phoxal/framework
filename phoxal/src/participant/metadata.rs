//! Participant metadata is part of the process ABI, not runner implementation.

pub use phoxal_runtime_contract::{
    PARTICIPANT_METADATA_SCHEMA, ParticipantKind, ParticipantMetadata as ParticipantMeta,
    parse_participant_metadata,
};
