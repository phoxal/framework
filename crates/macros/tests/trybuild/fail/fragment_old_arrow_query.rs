use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot / lookup;
    query current: Request => Response;
}

fn main() {}
