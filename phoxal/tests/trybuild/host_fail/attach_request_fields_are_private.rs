use phoxal::identity::ProducerId;
use phoxal::model::world::{WorldInstanceId, WorldProgress};
use phoxal::supervisor::api::simulation::attach::AttachRequest;

fn main() {
    let _ = AttachRequest {
        world: WorldInstanceId::mint(),
        controller: ProducerId::parse("10000000000000000000000000000001").unwrap(),
        progress: WorldProgress::at(1, 12).unwrap(),
    };
}
