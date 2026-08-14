use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot / drive;
        target: Inflow<Target>;
}

fn main() {}
