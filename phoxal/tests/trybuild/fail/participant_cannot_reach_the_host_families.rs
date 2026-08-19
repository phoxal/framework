// A participant authors against the robot family and sees no host domain. The
// runtime family is what a process says about itself - the runner emits it, and
// the world clock inside it belongs to whoever owns the world - and the
// supervisor family is the control plane an attached application speaks. Both
// are published by other profiles, so a participant cannot name either, let
// alone publish on one.
use phoxal::runtime::api::simulation::Clock;
use phoxal::supervisor::api::execution::Snapshot;

fn main() {
    let _ = std::mem::size_of::<(Clock, Snapshot)>();
}
