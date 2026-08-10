use phoxal_macros::{phoxal_api_fragment, phoxal_api_tree};

mod drive {
    use super::phoxal_api_fragment;
    phoxal_api_fragment! {
        path robot / drive;
        command target: Setpoint<Target>;
        command target: Setpoint<Target>;
    }
}

phoxal_api_tree! {
    output generated;
    source crate;
    fragments { drive; }
}

fn main() {}
