//! Framework-owned hand contracts.
//!
//! These endpoints are runtime plumbing, not robot API declarations.  They
//! are kept beside the runtime that owns their authority so a robot contract
//! tree cannot accidentally acquire the ability to mint framework time.

/// The external simulation hand-off endpoints.
pub mod simulation {
    use phoxal_bus::{
        ApiFamily, EndpointDescriptor, EndpointKind, EventContract, Publish,
        StreamDeliveryContract, Subscribe, Topic, WorldClockContract,
    };
    use serde::{Deserialize, Serialize};

    /// The runtime-owned API identity for hand contracts.
    #[doc(hidden)]
    pub enum Api {}

    impl ApiFamily for Api {
        const ID: &'static str = "runtime";
    }

    /// The body carried by the authoritative simulation clock hand.
    ///
    /// The production timeline and exact instant are bus metadata.  The body
    /// carries only the simulator's monotonic step counter.
    #[derive(Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize)]
    pub struct Clock {
        pub step: u64,
    }

    /// Runtime-owned descriptor for the authoritative simulation clock.
    ///
    /// This is deliberately an [`EndpointKind::Event`]: the hand is stamped
    /// at a completed world step, while its ordered stream transport preserves
    /// every accepted clock and reports gaps instead of silently coalescing.
    #[doc(hidden)]
    pub struct ClockEndpoint;

    impl EndpointDescriptor for ClockEndpoint {
        type Api = Api;
        type Payload = Clock;

        const NAME: &'static str = "runtime::simulation::Clock";
        const FAMILY: &'static str = "runtime";
        const CONTRACT: &'static str = "simulation::Clock";
        const TOPIC: &'static str = "runtime/simulation/clock";
        const KIND: EndpointKind = EndpointKind::Event;
    }

    impl EventContract for ClockEndpoint {}
    impl StreamDeliveryContract for ClockEndpoint {}
    impl WorldClockContract for ClockEndpoint {}

    /// The simulator's owner-side clock topic.
    #[doc(hidden)]
    pub fn owner_topic() -> Topic<Publish<ClockEndpoint>> {
        Topic::new_static(<ClockEndpoint as EndpointDescriptor>::TOPIC)
    }

    /// The participant-side clock topic.
    #[doc(hidden)]
    pub fn client_topic() -> Topic<Subscribe<ClockEndpoint>> {
        Topic::new_static(<ClockEndpoint as EndpointDescriptor>::TOPIC)
    }

    #[cfg(test)]
    mod tests {
        use super::*;

        #[test]
        fn clock_hand_is_runtime_owned_and_ordered() {
            assert_eq!(ClockEndpoint::FAMILY, "runtime");
            assert_eq!(ClockEndpoint::TOPIC, "runtime/simulation/clock");
            assert_eq!(ClockEndpoint::KIND, EndpointKind::Event);
            assert_eq!(owner_topic().key(), client_topic().key());
            assert_eq!(
                serde_json::to_value(Clock { step: 7 }).unwrap(),
                serde_json::json!({"step": 7})
            );
        }
    }
}
