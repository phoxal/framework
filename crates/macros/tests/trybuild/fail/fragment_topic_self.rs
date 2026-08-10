use phoxal_macros::phoxal_api_fragment;

phoxal_api_fragment! {
    path logs;
    version v1_0;
    topic self: Stream<Event>;
}

fn main() {}
