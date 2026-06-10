use phoxal::api::component::v1::capability::motor::Command;
use phoxal::api::v1::topic;
use phoxal::bus::topic::{PubSub, Topic};

fn want_command(_: Topic<PubSub<Command>>) {}

fn main() {
    want_command(topic::new().v1().drive().target());
}
