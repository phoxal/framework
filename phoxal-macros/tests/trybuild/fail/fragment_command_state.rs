use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path drive;
    version v1_0;
        command target: State<Target>;
}

fn main() {}
