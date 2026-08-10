use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path robot / logs;
    topic self: Stream<Event>;
}

fn main() {}
