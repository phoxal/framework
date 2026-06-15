use phoxal::api::component::capability::motor::Command;
use phoxal::api::topic;
use phoxal::bus::topic::{PubSub, Topic};

fn want_command(_: Topic<PubSub<Command>>) {}

fn main() {
    want_command(topic::new().drive().target());
}
