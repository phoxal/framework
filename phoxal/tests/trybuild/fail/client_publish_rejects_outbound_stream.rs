use phoxal::bus::ClientPublishContract;

trait PublishProof {
    fn prove();
}

impl<E: ClientPublishContract> PublishProof for E {
    fn prove() {}
}

fn main() {
    phoxal_protocol::supervisor::endpoint::logs::FollowEndpoint::prove();
}
