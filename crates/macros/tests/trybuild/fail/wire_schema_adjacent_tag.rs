//! An adjacently tagged enum is a representation this model does not carry.

#[allow(dead_code)]
#[derive(phoxal_macros::DescribeWire)]
#[serde(tag = "kind", content = "body")]
enum Split {
    Code(i32),
}

fn main() {}
