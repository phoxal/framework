/// Shared physical range contract.
pub use phoxal::robotics::RangeSample;

include!(concat!(env!("OUT_DIR"), "/phoxal.component.vl53l1x.v1.rs"));

/// Public typed ports owned by the VL53L1X component contract.
pub use vl53l1x::ports;

/// The original descriptor closure retained for independent contract
/// inspection and native artifact extraction.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));
