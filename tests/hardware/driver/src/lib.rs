//! Generated messages and ports for the synthetic hardware fixture.

/// Generated messages and typed ports owned by this fixture package.
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/phoxal.fixture.hardware.v1.rs"));
}

pub use generated::*;

/// The original descriptor closure retained for independent artifact
/// inspection.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

/// Public typed ports owned by the fixture contract.
pub use generated::hardware_fixture::ports;
