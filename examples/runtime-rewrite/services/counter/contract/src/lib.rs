include!(concat!(env!("OUT_DIR"), "/example.counter.v1.rs"));

/// The descriptor closure retained for package and artifact checks.
pub const FILE_DESCRIPTOR_SET: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/phoxal-descriptors.bin"));
