use phoxal::bus::{
    ClientPublishContract, ClientReceiveContract, Publish, StreamContract, Subscribe, Topic,
};

fn publish_stream<E: StreamContract + ClientPublishContract>(_: Topic<Publish<E>>) {}

fn receive_stream<E: StreamContract + ClientReceiveContract>(_: Topic<Subscribe<E>>) {}

fn main() {
    publish_stream(
        phoxal::api::topic::client()
            .component("speaker")
            .expect("valid component segment")
            .speaker("audio")
            .expect("valid capability segment")
            .stream(),
    );
    receive_stream(phoxal::supervisor::api::topic::client().logs().follow());
}
