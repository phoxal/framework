use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot(instance) / drive;
    state: State<State>;
}

fn main() {}
