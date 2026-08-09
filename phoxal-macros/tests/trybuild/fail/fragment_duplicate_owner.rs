use phoxal_macros::{phoxal_api_fragment, phoxal_api_fragment_group, phoxal_api_tree};

mod first {
    use super::phoxal_api_fragment;
    phoxal_api_fragment! {
        path drive;
        version v1_0;
            command target: Setpoint<Target>;
    }
}
mod second {
    use super::phoxal_api_fragment;
    phoxal_api_fragment! {
        path drive;
        version v1_0;
            topic state: State<State>;
    }
}
mod all {
    pub(super) use super::{first, second};
    use super::phoxal_api_fragment_group;
    phoxal_api_fragment_group! { fragments { first; second; } }
}

phoxal_api_tree! {
    output generated;
    source crate;
    versions { latest version v1_0; }
    fragments { all; }
}

fn main() {}
