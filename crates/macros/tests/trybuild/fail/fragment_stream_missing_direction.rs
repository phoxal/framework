use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot / audio;
    chunks: Stream<Chunk>;
}

fn main() {}
