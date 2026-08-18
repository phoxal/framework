use phoxal::bus::ClientReceiveContract;

trait ReceiveProof {
    fn prove();
}

impl<E: ClientReceiveContract> ReceiveProof for E {
    fn prove() {}
}

fn main() {
    phoxal::api::endpoint::component::speaker::StreamEndpoint::prove();
}
