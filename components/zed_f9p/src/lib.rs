include!(concat!(env!("OUT_DIR"), "/phoxal.component.zed_f9p.v1.rs"));

/// Public typed ports owned by the ZED-F9P component contract.
pub use zed_f9p::ports;

/// The original descriptor closure retained for independent contract
/// inspection and native artifact extraction.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));
