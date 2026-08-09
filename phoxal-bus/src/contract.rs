//! The contract primitive traits: the API-version marker and the
//! version-local wire body.
//!
//! These are the two traits the bus client is generic over - the ABI floor every
//! contract body and api-version marker implements. The concrete versioned API
//! versions (`phoxal::api`, …) and the `phoxal_api_tree!` macro that
//! generates their `ApiVersion` / `ContractBody` impls live in the `phoxal-api`
//! crate. This crate is their one canonical home. The `phoxal` engine re-exports
//! its authoring subset at `phoxal::bus`, so a participant author reaches them as
//! `phoxal::bus::ApiVersion` / `phoxal::bus::ContractBody`.

/// Marker trait identifying one generated contract tree.
///
/// Implemented only by the zero-variant `enum Api {}` that
/// [`phoxal_api_tree!`] generates inside each tree module. The [`ID`] is the
/// dotted wire revision (`"v0.1"`) for a robot API revision, and the protocol
/// name (`"supervisor"`) for a `protocol` tree - in both cases the tree's
/// identity and the leading segment of every key it declares. It is carried in
/// bus metadata as informational provenance, never in the wire body.
///
/// The marker's job is the same in both modes: it keeps one tree's bodies from
/// standing in for another's at compile time. `ParticipantSpec::ContractApi`
/// pins a participant to exactly one of them.
///
/// [`ID`]: ApiVersion::ID
/// [`phoxal_api_tree!`]: https://docs.rs/phoxal
pub trait ApiVersion: 'static {
    /// The tree's wire identifier: a dotted revision such as `"v0.1"` (Rust
    /// module `v0_1`), or a protocol name such as `"supervisor"`.
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

/// The semantic **and temporal** role a topic plays in its owning service's
/// contract.
///
/// Every topic in a [`phoxal_api_tree!`] declares exactly one of these.
/// The role records *intent*, separate from the wire shape: a `Command` and a
/// `State` topic are both pub/sub on the wire, but the owner subscribes a
/// `Command` (it is the service's control input) and publishes a `State` (it is
/// the service's telemetry output).
///
/// The role drives two things:
///
/// - the **side branding**: the api tree's builders read it to pick each
///   leaf's side-branded topic kind
///   (`Publish`/`Subscribe`/`AskQuery`/`ServeQuery`), so taking the wrong side
///   of a topic is a compile error;
/// - the **temporal capability**: the role decides which robot
///   time a publisher of that contract can express at all. A `State` is
///   published at a logical step, a `Measurement` carries a capture stamp, and
///   a `Command` or `Diagnostic` expresses no robot time. The generated body
///   implements exactly one of [`StateContract`] / [`MeasurementContract`] /
///   [`CommandContract`] / [`DiagnosticContract`], and each publisher handle is
///   bounded by its own marker, so reaching for the wrong publisher is a
///   compile error rather than a review question. One topic is the sole
///   exception: the framework's own `world_clock`-role `simulation::Clock`
///   wire-brands and reports `TopicRole::State` exactly like an ordinary state
///   topic, but implements the disjoint [`WorldClockContract`] instead of
///   `StateContract` - see that trait's docs for why.
///
/// [`phoxal_api_tree!`]: https://docs.rs/phoxal
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TopicRole {
    /// A control input the owning service subscribes (e.g. `drive/target`).
    /// Expresses no robot time: a command is a request, not an observation.
    Command,
    /// An ordered chunk input whose consumer must observe saturation or close
    /// rather than silently losing part of the stream.
    Stream,
    /// State the owning service publishes at a logical step (e.g.
    /// `drive/state`).
    State,
    /// A sensor observation the owning service publishes with a capture stamp
    /// (e.g. `component/{instance}/imu/{capability}/sample`).
    Measurement,
    /// An output that describes the participant rather than the world (health,
    /// logs, runtime evidence). Expresses no robot time.
    Diagnostic,
    /// A request/response topic the owning service answers (e.g. `map/submap`).
    Query,
}

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

    /// Derive a compatibility kind from the pre-endpoint contract metadata.
    ///
    /// This is only used by the transition implementation below.  New
    /// generated endpoint descriptors set [`EndpointDescriptor::KIND`]
    /// directly, so a payload can no longer accidentally select semantics.
    pub const fn from_legacy(role: TopicRole, delivery: DeliveryFamily) -> Self {
        match (role, delivery) {
            (TopicRole::Command, _) => Self::Setpoint,
            (TopicRole::Measurement, _) => Self::Sample,
            (TopicRole::Stream, _) => Self::Stream,
            // The existing framework logs endpoint is authored as a
            // diagnostic with ordered delivery. Preserve that meaning. A
            // legacy state-shaped endpoint with explicit ordered delivery
            // (joint state, events, or the world clock) likewise retains its
            // transport family until its endpoint descriptor is generated.
            (TopicRole::Diagnostic, DeliveryFamily::Stream) => Self::Event,
            (TopicRole::State, DeliveryFamily::Stream) => Self::Stream,
            (TopicRole::Query, _) => Self::Query,
            _ => Self::State,
        }
    }
}

/// Endpoint-owned identity and semantic descriptor.
///
/// `Payload` is the only wire body.  `TOPIC`, `KIND`, and the contract identity
/// are owned by this separate descriptor type, so reusing one payload in two
/// endpoints cannot silently reuse the first endpoint's transport behavior.
/// The API tree generator emits one descriptor per endpoint and implements the
/// semantic marker appropriate to [`KIND`].
pub trait EndpointDescriptor: 'static {
    /// The API tree or protocol this endpoint belongs to.
    type Api: ApiVersion;
    /// The plain serde payload carried by this endpoint.
    type Payload: Payload;
    /// Version-qualified endpoint identity.
    const NAME: &'static str;
    /// Endpoint tree identity, such as `"v0.1"` or `"supervisor"`.
    const VERSION: &'static str;
    /// Stable endpoint path within its tree.
    const CONTRACT: &'static str;
    /// Version-qualified concrete wire key template.
    const TOPIC: &'static str;
    /// Fixed semantic endpoint kind.
    const KIND: EndpointKind;
}

/// Short name for an endpoint descriptor used by typed bus APIs.
pub trait Endpoint: EndpointDescriptor {}

impl<T: EndpointDescriptor> Endpoint for T {}

impl TopicRole {
    /// The lowercase grammar keyword for this role, matching how it is written
    /// in `phoxal_api_tree!`.
    pub const fn as_str(self) -> &'static str {
        match self {
            TopicRole::Command => "command",
            TopicRole::Stream => "stream",
            TopicRole::State => "state",
            TopicRole::Measurement => "measurement",
            TopicRole::Diagnostic => "diagnostic",
            TopicRole::Query => "query",
        }
    }

    /// Map the authoring role to its delivery semantics.
    pub const fn delivery_family(self) -> DeliveryFamily {
        match self {
            Self::State | Self::Diagnostic => DeliveryFamily::State,
            Self::Measurement => DeliveryFamily::Sample,
            Self::Command => DeliveryFamily::Setpoint,
            Self::Stream => DeliveryFamily::Stream,
            Self::Query => DeliveryFamily::Query,
        }
    }
}

/// Marker for an endpoint whose temporal meaning is current state.
///
/// Generated by `phoxal_api_tree!` for every ordinary `state` topic. Deliberately
/// NOT implemented for the framework's own world-clock contract
/// (`phoxal::api::simulation::Clock`) - see [`WorldClockContract`] for why that
/// exclusion is the enforcement mechanism, not an oversight.
pub trait StateContract: EndpointDescriptor {}

/// Marker for the framework's single world-clock contract
/// (`phoxal::api::simulation::Clock`, generated by `phoxal_api_tree!`'s
/// `world_clock` topic role).
///
/// Deliberately a SIBLING of [`StateContract`], not a subtrait of it: if the
/// world clock also implemented `StateContract`, it would still satisfy the
/// ordinary, unrestricted `state_publisher` builder every participant has,
/// which would make "only a simulator can mint world steps" an unenforced
/// convention rather than a compiler rule.
/// Excluding it from `StateContract` is what makes that builder reject it at
/// compile time, forcing every caller through the world-authority-gated
/// `SetupContext::world_clock_publisher` in the `phoxal` crate
/// instead (`Self: world-authority surface`).
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

/// Compatibility marker for an endpoint whose temporal meaning is a captured
/// sample. New generated endpoints should implement [`SampleContract`]
/// directly; this name remains only while the old role grammar is migrated.
pub trait MeasurementContract: EndpointDescriptor {}

/// Compatibility marker for an endpoint whose temporal meaning is a
/// setpoint. New generated endpoints should implement [`SetpointContract`]
/// directly; this name remains only while the old role grammar is migrated.
pub trait CommandContract: EndpointDescriptor {}

/// Marker for an endpoint whose temporal meaning is an ordered stream chunk.
pub trait StreamContract: EndpointDescriptor {}

/// Marker for an endpoint whose temporal meaning is diagnostic evidence.
///
/// Generated by `phoxal_api_tree!`; it is the bound on
/// [`DiagnosticPublisher`](crate::handle::publisher::DiagnosticPublisher), which expresses
/// no robot time at all.
pub trait DiagnosticContract: EndpointDescriptor {}

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

/// A version-local wire body: a plain serde type bound to exactly one
/// [`ApiVersion`] and one contract topic.
///
/// Every body declared inside a `phoxal_api_tree!` node gets a generated impl.
/// Each body carries its own [`Api`](ContractBody::Api) version marker and
/// version-qualified [`TOPIC`](ContractBody::TOPIC).
/// Participant setup builders accept these bodies directly and preserve the
/// contract type through each typed handle.
///
/// The serde encoding of an implementor *is* the wire payload; there is no version
/// envelope.
///
/// **Wire identity is the key, not a hash.** The version is folded into
/// [`TOPIC`](ContractBody::TOPIC), so `v0.1::drive::Target` and a
/// hypothetically re-minted `v0.2::drive::Target` publish on different Zenoh
/// keys and physically cannot collide. Two participants interoperate on a
/// contract iff they use the exact same version-qualified name, which is
/// realized on the wire by the key.
/// A receiver's per-key Zenoh subscription is the
/// whole fast-reject; the bus decode path validates only the codec.
pub trait ContractBody:
    serde::Serialize + serde::de::DeserializeOwned + Send + Sync + 'static
{
    /// The single tree this body belongs to - one API revision, or one
    /// protocol. Two bodies from different trees have different `Api`, so the
    /// type system keeps them apart.
    type Api: ApiVersion;
    /// The version-qualified type identity: the dotted wire revision, then the `::`-joined node path
    /// (dynamic-node vars are topic params, never type-path segments), then
    /// the PascalCase type leaf, e.g. `"v0.1::drive::Target"` or
    /// `"v0.1::component::motor::Command"`. This is the contract's source
    /// identity - two contracts interoperate iff they share this exact
    /// name - as distinct from [`TOPIC`](ContractBody::TOPIC), the resolved
    /// wire key derived from it. `NAME` is exactly the `"::"`-join of
    /// [`VERSION`](ContractBody::VERSION) and
    /// [`CONTRACT`](ContractBody::CONTRACT); it stays available for callers
    /// that want the whole identity as one string (e.g. display), while
    /// consumers that must reason about the revision and the contract
    /// independently use the two split consts instead - a joined name is not
    /// machine-parseable without assuming the version naming scheme.
    const NAME: &'static str;
    /// This body's tree identity alone, e.g. `"v0.1"` - equal to
    /// `<Self::Api as ApiVersion>::ID`, but exposed directly on the body so a
    /// metadata or diagnostics recorder can const-splice it without routing
    /// through `Self::Api`.
    /// Split from [`CONTRACT`](ContractBody::CONTRACT) so a consumer can read
    /// the revision without parsing a joined name. In a `protocol` tree this is
    /// the protocol name: a protocol's payload version is a serde tag the
    /// document's author owns, not something the generator mints.
    const VERSION: &'static str;
    /// This body's contract path within its own version: the `::`-joined
    /// node path (dynamic-node vars excluded, as with `NAME`) plus the
    /// PascalCase type leaf, e.g. `"drive::Target"`. The **logical
    /// contract** - stable across a version bump - is this value alone;
    /// pairing it with [`VERSION`](ContractBody::VERSION) recovers the
    /// full version-qualified identity (`NAME`).
    const CONTRACT: &'static str;
    /// The tree-qualified wire key: the tree identity, then the `/`-joined
    /// node path plus the topic leaf, with each dynamic node contributing a
    /// `{var}` placeholder, e.g. `"v0.1/drive/state"`,
    /// `"v0.1/component/{instance}/motor/{capability}/command"`, or a
    /// protocol's `"supervisor/connect/hello"`. The concrete key is produced by
    /// the tree-local `topic` builder, which fills the placeholders, and the
    /// bus session mounts it under its execution-scoped root.
    const TOPIC: &'static str;
    /// The topic role this body was declared with, which fixes both the side
    /// brand and the robot time a publisher of it can express.
    const ROLE: TopicRole;
    /// Delivery semantics independent of temporal stamping.
    const DELIVERY: DeliveryFamily;
}

/// Compatibility descriptor for the legacy body-owned contract surface.
///
/// Keeping this implementation here lets the bus migrate independently from
/// the generated API crate: old bodies remain usable as `EndpointDescriptor`s,
/// while new generated endpoint marker types implement the descriptor trait
/// directly and carry a separate [`Payload`] type.
impl<T: ContractBody> EndpointDescriptor for T {
    type Api = T::Api;
    type Payload = T;

    const NAME: &'static str = T::NAME;
    const VERSION: &'static str = T::VERSION;
    const CONTRACT: &'static str = T::CONTRACT;
    const TOPIC: &'static str = T::TOPIC;
    const KIND: EndpointKind = EndpointKind::from_legacy(T::ROLE, T::DELIVERY);
}

// Compatibility bridges for the old generated body markers. New endpoint
// descriptors implement the endpoint-kind markers directly; these impls make
// the transition source-compatible without putting transport metadata back on
// an ordinary payload.
impl<T: MeasurementContract> SampleContract for T {}
impl<T: CommandContract> SetpointContract for T {}
impl<T: DiagnosticContract + StreamDeliveryContract> EventContract for T {}

#[cfg(test)]
mod tests {
    use super::*;

    #[derive(Debug, PartialEq, serde::Deserialize, serde::Serialize)]
    struct SharedPayload {
        value: u8,
    }

    enum TestApi {}

    impl ApiVersion for TestApi {
        const ID: &'static str = "endpoint-test";
    }

    struct StateEndpoint;
    struct EventEndpoint;

    impl EndpointDescriptor for StateEndpoint {
        type Api = TestApi;
        type Payload = SharedPayload;

        const NAME: &'static str = "endpoint-test::state";
        const VERSION: &'static str = "endpoint-test";
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
        const VERSION: &'static str = "endpoint-test";
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
