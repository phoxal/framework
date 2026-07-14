#![allow(unexpected_cfgs)]

use phoxal_macros::phoxal_api_tree;

phoxal_api_tree! {
    version v1 {
        sample {
            struct Body { value: u8 }
            topic body: state Body;
        }
    }

    preview version v2 {
        sample {
            struct Body { value: u8 }
            topic body: state Body;
        }
    }
}

fn main() {
    let _ = v2::topic::new();
}
