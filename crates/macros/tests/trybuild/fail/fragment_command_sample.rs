use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path robot / drive;
        command target: Sample<Target>;
}

fn main() {}
