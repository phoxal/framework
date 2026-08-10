use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path robot;
    topic state: State<State>;
}

fn main() {}
