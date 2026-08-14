use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot / drive;
    struct Target;
    target: Setpoint<Target>;
}

fn main() {}
