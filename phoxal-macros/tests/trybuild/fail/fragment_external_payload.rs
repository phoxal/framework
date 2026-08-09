use phoxal_macros::{phoxal_api_fragment, phoxal_api_tree};

struct Sample {
    value: u8,
}

mod data {
    use super::phoxal_api_fragment;

    phoxal_api_fragment! {
        path data;
        version v1_0;
            topic sample: State<crate::Sample>;
    }
}

phoxal_api_tree! {
    output generated;
    source crate;
    versions { latest version v1_0; }
    fragments { data; }
}

fn main() {}
