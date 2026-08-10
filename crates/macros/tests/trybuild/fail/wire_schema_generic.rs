//! One generic declaration stands for a family of different wire shapes.

#[allow(dead_code)]
#[derive(phoxal_macros::DescribeWire)]
struct Bounded<const MAX: usize>(String);

fn main() {}
