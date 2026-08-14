use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path telemetry / drive;
    state: State<State>;
}

fn main() {}
