use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot;
    topic state: State<State>;
}

fn main() {}
