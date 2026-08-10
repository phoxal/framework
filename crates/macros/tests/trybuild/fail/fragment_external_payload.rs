use phoxal_macros::{phoxal_api_fragment, phoxal_api_tree};

struct Sample {
    value: u8,
}

mod data {
    use super::phoxal_api_fragment;

    phoxal_api_fragment! {
        path robot / data;
            topic sample: State<crate::Sample>;
    }
}

phoxal_api_tree! {
    output generated;
    source crate;
    fragments { data; }
}

fn main() {}
