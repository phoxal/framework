// A participant authors against the robot family and sees no host domain. The
// simulation family carries progress published by an attached world, and the
// supervisor family is the control plane an attached application speaks.
// Those families are published by other profiles, so a participant cannot name
// either, let alone publish on one.
use phoxal::simulation::api::StepEvent;
use phoxal::supervisor::api::execution::Snapshot;

fn main() {
    let _ = std::mem::size_of::<(StepEvent, Snapshot)>();
}
