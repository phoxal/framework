use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path drive;
    version v1_0;
        replace impl Target {}
        command target: Setpoint<Target>;
}

fn main() {}
