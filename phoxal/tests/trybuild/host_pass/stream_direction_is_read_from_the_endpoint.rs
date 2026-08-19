// A stream's direction is owner-relative and is read straight off the
// endpoint's own semantic, so a generic client distinguishes an inbound from an
// outbound stream without an endpoint allowlist beside it.
use phoxal::bus::{Endpoint, In, Out, Publish, Stream, Subscribe, Topic};
use phoxal::identity::ComponentInstanceId;
use phoxal::model::identity::CapabilityId;

fn publish_inbound_stream<E: Endpoint<Semantics = Stream<In>>>(_: Topic<Publish<E>>) {}

fn receive_outbound_stream<E: Endpoint<Semantics = Stream<Out>>>(_: Topic<Subscribe<E>>) {}

fn main() {
    let speaker = ComponentInstanceId::new("speaker").expect("valid component segment");
    let audio = CapabilityId::new("audio").expect("valid capability segment");
    publish_inbound_stream(
        phoxal::api::topics()
            .component(&speaker)
            .expect("valid component segment")
            .speaker(&audio)
            .expect("valid capability segment")
            .stream()
            .client(),
    );
    receive_outbound_stream(phoxal::supervisor::api::topics().logs().follow().client());
}
