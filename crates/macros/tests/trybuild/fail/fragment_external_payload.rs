use phoxal_macros::{protocol_fragment, protocol_tree};

struct Sample {
    value: u8,
}

mod data {
    use super::protocol_fragment;

    protocol_fragment! {
        path robot / data;
            topic sample: State<crate::Sample>;
    }
}

protocol_tree! {
    output generated;
    source crate;
    fragments { data; }
}

fn main() {}
