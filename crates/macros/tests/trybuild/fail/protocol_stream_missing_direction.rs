use phoxal_macros::protocol_tree;

protocol_tree! {
    path runtime / logs;
    self: Stream<Event>;
}

struct Event;

fn main() {}
