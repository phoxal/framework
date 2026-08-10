//! A flattened field's keys come from another type's declaration, so this one
//! does not state its own wire shape.

#[allow(dead_code)]
#[derive(phoxal_macros::DescribeWire)]
struct Inner {
    value: u32,
}

#[allow(dead_code)]
#[derive(phoxal_macros::DescribeWire)]
struct Outer {
    #[serde(flatten)]
    inner: Inner,
}

fn main() {}
