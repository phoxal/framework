use phoxal_macros::{protocol_fragment, protocol_tree};

mod drive {
    use super::protocol_fragment;
    protocol_fragment! {
        path robot / drive;
        target: Setpoint<Target>;
        target: Setpoint<Target>;
    }
}

protocol_tree! {
    output generated;
    source crate;
    fragments { drive; }
}

fn main() {}
