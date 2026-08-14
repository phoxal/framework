use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot(instance) / drive;
    topic state: State<State>;
}

fn main() {}
