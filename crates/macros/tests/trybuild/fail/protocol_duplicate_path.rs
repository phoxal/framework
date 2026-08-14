use phoxal_macros::protocol_tree;

protocol_tree! {
    path robot / drive;
    state: State<State>;
    path robot / drive;
    target: Setpoint<Target>;
}

struct State;
struct Target;

fn main() {}
