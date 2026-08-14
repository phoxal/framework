use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot / lookup;
    current: Query<Request>;
}

fn main() {}
