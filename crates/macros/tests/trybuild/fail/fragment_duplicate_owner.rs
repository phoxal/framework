use phoxal_macros::{protocol_fragment, protocol_fragment_group, protocol_tree};

mod first {
    use super::protocol_fragment;
    protocol_fragment! {
        path robot / drive;
            target: Setpoint<Target>;
    }
}
mod second {
    use super::protocol_fragment;
    protocol_fragment! {
        path robot / drive;
            state: State<State>;
    }
}
mod all {
    pub(super) use super::{first, second};
    use super::protocol_fragment_group;
    protocol_fragment_group! { fragments { first; second; } }
}

protocol_tree! {
    output generated;
    source crate;
    fragments { all; }
}

fn main() {}
