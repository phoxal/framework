use phoxal::bus::ClientReceiveContract;

trait ReceiveProof {
    fn prove();
}

impl<E: ClientReceiveContract> ReceiveProof for E {
    fn prove() {}
}

fn main() {
    phoxal_protocol::robot::endpoint::component::speaker::StreamEndpoint::prove();
}
