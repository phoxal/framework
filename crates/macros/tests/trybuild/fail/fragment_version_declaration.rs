use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot / drive;
    version v0_1;
    command target: Setpoint<Target>;
}

fn main() {}
