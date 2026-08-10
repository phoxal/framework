//! An arbitrary serializer writes a shape the declaration does not state.

#[allow(dead_code)]
#[derive(phoxal_macros::DescribeWire)]
struct Custom {
    #[serde(serialize_with = "write_it")]
    value: u32,
}

fn main() {}
