use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot / drive;
        command target: State<Target>;
}

fn main() {}
