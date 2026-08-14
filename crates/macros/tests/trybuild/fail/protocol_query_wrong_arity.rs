use phoxal_macros::protocol_tree;

protocol_tree! {
    path supervisor / info;
    topic: Query<Request>;
}

struct Request;

fn main() {}
