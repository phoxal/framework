use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path telemetry / drive;
    topic state: State<State>;
}

fn main() {}
