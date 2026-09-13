include!(concat!(env!("OUT_DIR"), "/phoxal.component.bno085.v1.rs"));

/// Public typed ports owned by the BNO085 component contract.
pub use bno085::ports;

/// The original descriptor closure retained for independent contract
/// inspection and native artifact extraction.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));
