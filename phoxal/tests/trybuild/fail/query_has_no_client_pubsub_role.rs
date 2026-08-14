use phoxal::bus::{ClientPublishContract, ClientReceiveContract};

trait PublishProof {
    fn prove_publish();
}

impl<E: ClientPublishContract> PublishProof for E {
    fn prove_publish() {}
}

trait ReceiveProof {
    fn prove_receive();
}

impl<E: ClientReceiveContract> ReceiveProof for E {
    fn prove_receive() {}
}

type Query = phoxal_protocol::supervisor::endpoint::bundle::GetEndpoint;

fn main() {
    Query::prove_publish();
    Query::prove_receive();
}
