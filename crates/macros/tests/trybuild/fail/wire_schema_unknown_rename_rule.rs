//! Rename rules are serde's own vocabulary; an unknown one is refused there.

#[allow(dead_code)]
#[derive(phoxal_macros::DescribeWire)]
#[serde(rename_all = "TitleCase")]
struct Sample {
    value: u32,
}

fn main() {}
