include!(concat!(env!("OUT_DIR"), "/phoxal.component.ddsm115.v1.rs"));

/// Public typed ports owned by the DDSM115 component contract.
pub use ddsm115::ports;

/// The original descriptor closure retained for independent contract
/// inspection and native artifact extraction.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));

pub use phoxal_robotics::EncoderSample;
