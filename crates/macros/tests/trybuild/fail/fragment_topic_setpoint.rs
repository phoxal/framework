use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot / drive;
        topic target: Setpoint<Target>;
}

fn main() {}
