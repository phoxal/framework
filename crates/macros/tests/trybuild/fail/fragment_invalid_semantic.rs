use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path robot / drive;
        command target: State<Target>;
}

fn main() {}
