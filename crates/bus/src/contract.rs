//! The contract primitive traits: the contract-family marker and endpoint
//! descriptors.
//!
//! These are the two traits the bus client is generic over - the ABI floor
//! every contract family uses: ordinary Rust payloads plus one generated
//! descriptor per endpoint. This crate owns the shared ABI traits; `phoxal-api`
//! owns every concrete declaration.

/// Marker trait identifying one generated contract tree.
///
/// Implemented only by the zero-variant `enum Api {}` that `phoxal_api_tree!`
/// generates inside each family module. The [`ID`] is a semantic contract
/// namespace - `"robot"`, `"runtime"`, `"supervisor"` - and is both the tree's
/// identity and the leading segment of every key it declares. It is carried in
/// bus metadata as informational provenance, never in the wire body.
///
/// A family names meaning, not a revision. Compatibility between two
/// participants is the framework train version they were built from, compared
/// for exact equality, so no key or descriptor carries a per-API version.
///
/// The marker keeps one family's bodies from standing in for another's at
/// compile time. `ParticipantSpec::ContractApi` pins a participant to exactly
/// one of them.
///
/// [`ID`]: ApiFamily::ID
pub trait ApiFamily: 'static {
    /// The family's wire identifier, such as `"robot"`.
    const ID: &'static str;
}

/// A plain serde payload carried by one bus endpoint.
///
/// Payloads deliberately contain no transport identity or delivery policy.
/// Those facts belong to an [`EndpointDescriptor`], which is the type used by
/// typed topics and handles.  The blanket implementation keeps ordinary
/// structs and enums frictionless: an author only derives serde for a payload
/// and never has to repeat a topic, role, or queue policy on the payload type.
pub trait Payload: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static {}

impl<T> Payload for T where T: serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{}

/// The minimal transport semantic family a contract requires.
///
/// Temporal stamping is intentionally separate: a `sample` may carry a device
/// capture window while a `state` is stamped by the runner's logical step.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeliveryFamily {
    /// Retain the newest observable snapshot.
    State,
    /// Preserve bounded ordered observations with explicit loss evidence.
    Sample,
    /// Retain only the newest actionable intent.
    Setpoint,
    /// Preserve ordered chunks and surface saturation/gaps.
    Stream,
    /// Bounded immediate lookup/admission.
    Query,
}

impl DeliveryFamily {
    /// The canonical spelling of this family.
    ///
    /// A delivery family is compile-time typing: it selects a transport lane
    /// and never becomes bytes of its own. The spelling exists so an endpoint
    /// record in a contract surface can name the lane it rides without a second
    /// vocabulary being invented beside this enum.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Sample => "sample",
            Self::Setpoint => "setpoint",
            Self::Stream => "stream",
            Self::Query => "query",
        }
    }
}

/// The fixed semantic kind of an endpoint.
///
/// This is endpoint metadata, not payload metadata.  The five pub/sub kinds
/// intentionally have no user-selectable queue policy: their bus behavior is
/// fixed by the kind.  `Query` remains the bounded request/reply path rather
/// than an outbound scheduler lane.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum EndpointKind {
    /// A current state snapshot, stamped at a logical step.
    State,
    /// A captured observation, ordered with explicit bounded loss evidence.
    Sample,
    /// A state-temporal event, ordered and gap-observable.
    Event,
    /// An ordered stream chunk with refusal-preserving admission.
    Stream,
    /// A newest-actionable intent, coalesced before transport.
    Setpoint,
    /// A bounded request/reply endpoint.
    Query,
}

impl EndpointKind {
    /// The canonical spelling of this kind, for a contract-surface record.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Sample => "sample",
            Self::Event => "event",
            Self::Stream => "stream",
            Self::Setpoint => "setpoint",
            Self::Query => "query",
        }
    }

    /// The transport family fixed by this endpoint kind.
    pub const fn delivery_family(self) -> DeliveryFamily {
        match self {
            Self::State => DeliveryFamily::State,
            Self::Sample => DeliveryFamily::Sample,
            Self::Event | Self::Stream => DeliveryFamily::Stream,
            Self::Setpoint => DeliveryFamily::Setpoint,
            Self::Query => DeliveryFamily::Query,
        }
    }
}

/// Endpoint-owned identity and semantic descriptor.
///
/// `Payload` is the only wire body.  `TOPIC`, `KIND`, and the contract identity
/// are owned by this separate descriptor type, so reusing one payload in two
/// endpoints cannot silently reuse the first endpoint's transport behavior.
/// The API tree generator emits one descriptor per endpoint and implements the
/// semantic marker appropriate to [`EndpointDescriptor::KIND`].
pub trait EndpointDescriptor: 'static {
    /// The contract family this endpoint belongs to.
    type Api: ApiFamily;
    /// The plain serde payload carried by this endpoint.
    type Payload: Payload;
    /// Family-qualified endpoint identity.
    const NAME: &'static str;
    /// Endpoint tree identity, such as `"robot"` or `"supervisor"`.
    const FAMILY: &'static str;
    /// Stable endpoint path within its tree.
    const CONTRACT: &'static str;
    /// Family-rooted concrete wire key template.
    const TOPIC: &'static str;
    /// Fixed semantic endpoint kind.
    const KIND: EndpointKind;
}

/// Descriptor for a typed request/reply endpoint.
///
/// A semantic endpoint owns both payload paths while remaining one zero-sized
/// type in the generated tree.
pub trait QueryEndpointDescriptor: EndpointDescriptor {
    /// Request payload decoded from the query.
    type Request: Payload;
    /// Response payload encoded in the reply.
    type Response: Payload;
}

/// Short name for an endpoint descriptor used by typed bus APIs.
pub trait Endpoint: EndpointDescriptor {}

impl<T: EndpointDescriptor> Endpoint for T {}

/// Marker for an endpoint whose temporal meaning is current state.
///
/// Generated by `phoxal_api_tree!` for every ordinary `State<T>` endpoint. Deliberately
/// NOT implemented for the runtime-owned world-clock hand - see
/// [`WorldClockContract`] for why that exclusion is the enforcement mechanism,
/// not an oversight.
pub trait StateContract: EndpointDescriptor {}

/// Marker for the framework's single runtime-owned world-clock hand.
///
/// Deliberately a SIBLING of [`StateContract`], not a subtrait of it: if the
/// world clock also implemented `StateContract`, it would still satisfy the
/// ordinary, unrestricted `state_publisher` builder every participant has,
/// which would make "only a simulator can mint world steps" an unenforced
/// convention rather than a compiler rule.
/// Excluding it from `StateContract` is what makes that builder reject it at
/// compile time, forcing every caller through the world-authority-gated
/// `SetupContext::world_clock_publisher` in the `phoxal` crate instead
/// (`Self: world-authority surface`).
///
/// Bounds [`WorldClockPublisher`](crate::handle::publisher::WorldClockPublisher), a
/// dedicated handle type separate from
/// [`StatePublisher`](crate::handle::publisher::StatePublisher) even though both publish
/// at a logical step with the same [`StepStamp`](crate::handle::stamp::StepStamp)
/// path: sharing one generic handle type across both traits would force
/// `StatePublisher`'s bound onto a common supertrait, which would blur an
/// ordinary participant's "wrong contract for `StatePublisher`" compile error
/// (today a precise `B: StateContract` message with the real `state` topics
/// listed as candidates) into a less legible one naming an internal plumbing
/// trait instead. Two small handle types keep that diagnostic exact.
pub trait WorldClockContract: EndpointDescriptor {}

/// Marker for an endpoint whose temporal meaning is an ordered stream chunk.
pub trait StreamContract: EndpointDescriptor {}

/// Marker for a state-temporal, ordered event endpoint.
///
/// Events use the stream transport's ordered/gap-visible behavior but are
/// stamped like state at a logical step. Generated endpoint descriptors should
/// implement this marker and `StreamDeliveryContract`; the payload remains a
/// plain serde type.
pub trait EventContract: EndpointDescriptor + StreamDeliveryContract {}

/// Marker for the endpoint kind `Sample`.
pub trait SampleContract: EndpointDescriptor {}

/// Marker for the endpoint kind `Setpoint`.
pub trait SetpointContract: EndpointDescriptor {}

/// Marker for a contract whose transport retains the newest state snapshot.
///
/// This is deliberately independent from the temporal publisher marker above:
/// a diagnostic or event can use state-temporal stamping while requiring an
/// ordered transport family.
pub trait StateDeliveryContract: EndpointDescriptor {}

/// Marker for a contract whose transport preserves bounded ordered samples
/// with explicit loss evidence.
pub trait SampleDeliveryContract: EndpointDescriptor {}

/// Marker for a contract whose transport retains only the newest actionable
/// intent.
pub trait SetpointDeliveryContract: EndpointDescriptor {}

/// Marker for a contract whose transport preserves ordered chunks and surfaces
/// saturation rather than evicting an older chunk.
pub trait StreamDeliveryContract: EndpointDescriptor {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Deserialize, serde::Serialize)]
    struct SharedPayload {
        value: u8,
    }

    enum TestApi {}

    impl ApiFamily for TestApi {
        const ID: &'static str = "endpoint-test";
    }

    struct StateEndpoint;
    struct EventEndpoint;

    impl EndpointDescriptor for StateEndpoint {
        type Api = TestApi;
        type Payload = SharedPayload;

        const NAME: &'static str = "endpoint-test::state";
        const FAMILY: &'static str = "endpoint-test";
        const CONTRACT: &'static str = "state";
        const TOPIC: &'static str = "endpoint-test/state";
        const KIND: EndpointKind = EndpointKind::State;
    }

    impl StateContract for StateEndpoint {}
    impl StateDeliveryContract for StateEndpoint {}

    impl EndpointDescriptor for EventEndpoint {
        type Api = TestApi;
        type Payload = SharedPayload;

        const NAME: &'static str = "endpoint-test::event";
        const FAMILY: &'static str = "endpoint-test";
        const CONTRACT: &'static str = "event";
        const TOPIC: &'static str = "endpoint-test/event";
        const KIND: EndpointKind = EndpointKind::Event;
    }

    impl EventContract for EventEndpoint {}
    impl StreamDeliveryContract for EventEndpoint {}

    fn accepts_state_endpoint<E: EndpointDescriptor<Payload = SharedPayload>>() {}

    fn accepts_event_handles(
        _: Option<crate::handle::publisher::EventPublisher<EventEndpoint>>,
        _: Option<crate::handle::subscriber::EventReceiver<EventEndpoint>>,
    ) {
    }

    #[test]
    fn one_plain_payload_can_be_reused_by_distinct_endpoint_descriptors() {
        accepts_state_endpoint::<StateEndpoint>();
        accepts_state_endpoint::<EventEndpoint>();
        accepts_event_handles(None, None);

        assert_eq!(StateEndpoint::KIND, EndpointKind::State);
        assert_eq!(EventEndpoint::KIND, EndpointKind::Event);
        assert_ne!(StateEndpoint::TOPIC, EventEndpoint::TOPIC);
        assert_eq!(
            EndpointKind::Event.delivery_family(),
            DeliveryFamily::Stream
        );
    }
}
