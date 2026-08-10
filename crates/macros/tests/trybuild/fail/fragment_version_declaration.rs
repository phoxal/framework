use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path robot / drive;
    version v0_1;
    command target: Setpoint<Target>;
}

fn main() {}
