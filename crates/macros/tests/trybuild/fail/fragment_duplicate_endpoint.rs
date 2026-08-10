use phoxal_macros::{phoxal_api_fragment, phoxal_api_tree};

mod drive {
    use super::phoxal_api_fragment;
    phoxal_api_fragment! {
        path drive;
        version v1_0;
        command target: Setpoint<Target>;
        command target: Setpoint<Target>;
    }
}

phoxal_api_tree! {
    output generated;
    source crate;
    versions { latest version v1_0; }
    fragments { drive; }
}

fn main() {}
