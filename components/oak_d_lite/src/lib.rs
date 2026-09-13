include!(concat!(
    env!("OUT_DIR"),
    "/phoxal.component.oak_d_lite.v1.rs"
));

/// Public typed ports owned by the OAK-D Lite component contract.
pub use oak_d_lite::ports;

/// The original descriptor closure retained for independent contract
/// inspection and native artifact extraction.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));
