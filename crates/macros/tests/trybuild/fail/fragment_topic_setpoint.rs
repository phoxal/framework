use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path robot / drive;
        topic target: Setpoint<Target>;
}

fn main() {}
