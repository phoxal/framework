use phoxal::port::{PortDescriptor, PortKind, State};

struct Payload;

const STATUS: State<Payload> = State::new("status");

fn main() {
    assert_eq!(STATUS.name(), "status");
    assert_eq!(<State<Payload> as PortDescriptor>::KIND, PortKind::State);
}
