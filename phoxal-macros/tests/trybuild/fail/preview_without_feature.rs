#![allow(unexpected_cfgs)]

use phoxal_macros::phoxal_api_tree;

phoxal_api_tree! {
    version y2026_1 {
        sample {
            struct Body { value: u8 }
            topic body: state Body;
        }
    }

    preview version y2026_2 extends y2026_1 {}
}

fn main() {
    let _ = y2026_2::topic::new();
}
