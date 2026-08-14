use phoxal_macros::protocol_tree;

protocol_tree! {
    path robot / drive;
    state: Unknown<State>;
}

struct State;

fn main() {}
