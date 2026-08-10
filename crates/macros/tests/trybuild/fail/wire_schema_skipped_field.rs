//! A skipped field is not part of the shape either side agrees about.

#[allow(dead_code)]
#[derive(phoxal_macros::DescribeWire)]
struct Hidden {
    value: u32,
    #[serde(skip)]
    local: u32,
}

fn main() {}
