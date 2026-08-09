use phoxal_macros::{phoxal_api_fragment, phoxal_api_tree};

mod base {
    use super::phoxal_api_fragment;
    phoxal_api_fragment! {
        path drive;
        version v1_0;
        command target: Setpoint<Target>;
    }
}

mod delta {
    use super::phoxal_api_fragment;
    phoxal_api_fragment! {
        path drive;
        version v1_1;
        remove endpoint target;
        command target: Setpoint<Target>;
    }
}

phoxal_api_tree! {
    output generated;
    source crate;
    versions {
        version v1_0;
        latest version v1_1 extends v1_0;
    }
    fragments { base; delta; }
}

fn main() {}
