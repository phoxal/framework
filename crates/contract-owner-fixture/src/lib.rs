//! Independent owner-side contract fixture.

/// The generated messages and service-owned typed ports.
pub mod generated {
    include!(concat!(env!("OUT_DIR"), "/example.inspection.v1.rs"));
}

pub use generated::*;

/// The original standard `FileDescriptorSet`, including imported options.
pub const DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));
