#![deny(dead_code)]

use phoxal_macros::phoxal_api_fragment;

mod forgotten {
    use super::phoxal_api_fragment;
    phoxal_api_fragment! {
        path forgotten;
        version v1_0;
            topic state: State<State>;
    }
}

fn main() {}
