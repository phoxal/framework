// One path tree, one side chosen at the endpoint: a publisher handle takes the
// side that publishes, and the client side of a state endpoint subscribes. The
// brand is what refuses it, before any key is looked at.
fn state_publisher_over_the_client_side(bus: phoxal::bus::BusHandle) {
    let _ = phoxal::bus::StatePublisher::new(
        bus,
        &phoxal::api::topics().drive().state().client(),
    );
}

fn main() {}
