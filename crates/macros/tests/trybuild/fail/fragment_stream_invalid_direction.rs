use phoxal_macros::protocol_fragment;

protocol_fragment! {
    path robot / audio;
    chunks: Stream<Chunk, Sideways>;
}

fn main() {}
