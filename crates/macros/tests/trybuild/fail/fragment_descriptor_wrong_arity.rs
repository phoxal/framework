use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot / state;
    current: State<Snapshot, Extra>;
}

fn main() {}
