#![deny(dead_code)]

use phoxal_macros::protocol_fragment;

mod forgotten {
    use super::protocol_fragment;
    protocol_fragment! {
        path robot / forgotten;
            state: State<State>;
    }
}

fn main() {}
