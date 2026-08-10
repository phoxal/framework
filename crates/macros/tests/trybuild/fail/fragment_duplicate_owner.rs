use phoxal_macros::{phoxal_api_fragment, phoxal_api_fragment_group, phoxal_api_tree};

mod first {
    use super::phoxal_api_fragment;
    phoxal_api_fragment! {
        path robot / drive;
            command target: Setpoint<Target>;
    }
}
mod second {
    use super::phoxal_api_fragment;
    phoxal_api_fragment! {
        path robot / drive;
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
    fragments { all; }
}

fn main() {}
