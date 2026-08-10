use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path drive;
    version v1_0;
        remove drive::target;
        command target: Setpoint<Target>;
}

fn main() {}
