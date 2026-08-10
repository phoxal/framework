use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path robot / drive;
    struct Target;
    command target: Setpoint<Target>;
}

fn main() {}
