//! The contract vocabulary the bus is generic over: wire families, endpoint
//! semantics, and the endpoint trait itself.
//!
//! There is exactly one Rust identity per endpoint, and it is the payload (or,
//! for a query, the request) type the endpoint carries. An endpoint is not a
//! separate descriptor beside its body: it *is* the body, with a [`Family`] and
//! an [`EndpointSemantics`] attached. Both attachments are sealed and are
//! written only by the `endpoints!` declaration that sits beside the
//! payload, so a hand-written implementation cannot invent an endpoint the
//! compatibility records do not know about.
//!
//! # Semantics, not markers
//!
//! [`EndpointSemantics`] is a closed set - [`State`], [`Sample`], [`Event`],
//! [`Setpoint`], [`Stream<In>`](Stream), [`Stream<Out>`](Stream), [`Query`],
//! [`WorldClock`] - and each member fixes the wire [`EndpointKind`], the
//! transport [`DeliveryFamily`], and the two side brands a client and an owner
//! respectively receive. A handle bounds itself on the semantics it serves
//! (`StatePublisher<E: Endpoint<Semantics = State>>`), so taking the wrong
//! operation on an endpoint is a compile error naming the endpoint's own
//! semantics.
//!
//! [`WorldClock`] is the one semantic whose authority differs from its wire
//! shape: it rides the same ordered stream transport as an [`Event`] and
//! declares the same [`EndpointKind::Event`], but it is a distinct semantic so
//! that the ordinary state publisher every participant has cannot mint a world
//! step.

use std::marker::PhantomData;

use crate::bus::topic::{AskQuery, Publish, ServeQuery, Subscribe, TopicKind};

/// The sealing supertraits.
///
/// They live in a crate-private module, so an implementation outside `phoxal`
/// cannot name them and therefore cannot implement the public traits that
/// require them. Inside the crate the only writer is the `endpoints!`
/// declaration, beside the payload it declares.
pub(crate) mod sealed {
    pub trait Endpoint {}
    pub trait Semantics {}
    pub trait Family {}
    pub trait Direction {}
}

/// A plain serde payload carried by one bus endpoint.
///
/// Payloads contain no transport identity and no delivery policy: those are
/// [`Endpoint`]'s two associated types. The blanket implementation keeps
/// ordinary structs and enums frictionless - an author derives serde for a
/// payload and never repeats a topic, role, or queue policy on it.
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
    #[must_use]
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

/// One wire family: a semantic namespace, and the leading segment of every key
/// declared below it.
///
/// A family names meaning, not a revision. Compatibility between two
/// participants is the compatibility line of the framework trains they were
/// built from, so no key or endpoint carries a per-API version.
///
/// The four members are the whole set, and the trait is sealed: a family is a
/// wire namespace this framework owns, not an extension point.
pub trait Family: sealed::Family + 'static {
    /// The family's wire identifier, such as `"robot"`.
    const ID: &'static str;
}

/// The robot domain a participant authors against, reached as `phoxal::api`
/// by the profiles that publish it.
pub enum Robot {}

/// What a running Phoxal process says about itself, reached as
/// `phoxal::runtime::api` by the profiles that publish it.
pub enum Runtime {}

/// The wire vocabulary a supervisor speaks, reached as
/// `phoxal::supervisor::api` by the profiles that publish it.
pub enum Supervisor {}

/// World-progress contracts, reached as `phoxal::simulation::api` by host
/// profiles. This is distinct from `phoxal::simulator`, the Rust host SDK.
pub enum Simulation {}

impl sealed::Family for Robot {}
impl Family for Robot {
    const ID: &'static str = "robot";
}

impl sealed::Family for Runtime {}
impl Family for Runtime {
    const ID: &'static str = "runtime";
}

impl sealed::Family for Supervisor {}
impl Family for Supervisor {
    const ID: &'static str = "supervisor";
}

impl sealed::Family for Simulation {}
impl Family for Simulation {
    const ID: &'static str = "simulation";
}

/// The stand-in family the bus's own unit tests declare endpoints in.
///
/// The bus is the ABI floor and has to be exercisable without the generated
/// tree above it, so its unit tests declare their own endpoints - under their
/// own family, so a stand-in can never be mistaken for a real one.
#[cfg(test)]
pub(crate) enum TestFamily {}

#[cfg(test)]
impl sealed::Family for TestFamily {}

#[cfg(test)]
impl Family for TestFamily {
    const ID: &'static str = "yTEST";
}

/// Which way a stream flows, relative to the endpoint's owner.
pub trait Direction: sealed::Direction + 'static {}

/// Into the owner: the external client publishes, the owner consumes.
pub enum In {}

/// Out of the owner: the owner publishes, the external client consumes.
pub enum Out {}

impl sealed::Direction for In {}
impl Direction for In {}
impl sealed::Direction for Out {}
impl Direction for Out {}

/// The current state of the endpoint's owner, stamped at a logical step.
pub enum State {}

/// A captured device observation, carrying its own capture stamp.
pub enum Sample {}

/// A discrete, ordered, gap-observable occurrence stamped at a logical step.
pub enum Event {}

/// Newest-actionable intent sent to the endpoint's owner.
pub enum Setpoint {}

/// An ordered chunk stream flowing in direction `D`, relative to the owner.
pub struct Stream<D: Direction>(PhantomData<fn() -> D>);

/// A bounded request/reply exchange.
pub enum Query {}

/// The framework's one world-clock hand.
///
/// A distinct authority semantic with the wire shape of an [`Event`]: it is
/// excluded from every ordinary publisher bound, so the only way to mint a
/// world step is the dedicated world-clock publisher no participant reaches.
pub enum WorldClock {}

/// What one endpoint semantic fixes.
///
/// The wire kind, the transport lane, and the two side brands all follow from
/// the semantic alone, so a declaration states the semantic and nothing else
/// and the four facts cannot drift apart.
pub trait EndpointSemantics: sealed::Semantics + 'static {
    /// The wire kind this semantic declares.
    const KIND: EndpointKind;
    /// The transport lane the kind selects.
    const DELIVERY: DeliveryFamily = Self::KIND.delivery_family();
    /// The brand an external client of the endpoint receives.
    type Client<E: Endpoint>: TopicKind;
    /// The brand the endpoint's owner receives - always the mirror of
    /// [`Client`](Self::Client).
    type Owner<E: Endpoint>: TopicKind;
}

/// The semantics that ride the ordered stream transport.
///
/// This is what a [`StreamReceiver`](crate::bus::StreamReceiver) accepts:
/// ordered delivery with visible gaps and refusal-preserving admission, in
/// either direction, plus the two step-stamped semantics that share that lane.
pub trait StreamDelivered: EndpointSemantics {}

impl sealed::Semantics for State {}
impl EndpointSemantics for State {
    const KIND: EndpointKind = EndpointKind::State;
    type Client<E: Endpoint> = Subscribe<E>;
    type Owner<E: Endpoint> = Publish<E>;
}

impl sealed::Semantics for Sample {}
impl EndpointSemantics for Sample {
    const KIND: EndpointKind = EndpointKind::Sample;
    type Client<E: Endpoint> = Subscribe<E>;
    type Owner<E: Endpoint> = Publish<E>;
}

impl sealed::Semantics for Event {}
impl EndpointSemantics for Event {
    const KIND: EndpointKind = EndpointKind::Event;
    type Client<E: Endpoint> = Subscribe<E>;
    type Owner<E: Endpoint> = Publish<E>;
}
impl StreamDelivered for Event {}

impl sealed::Semantics for Setpoint {}
impl EndpointSemantics for Setpoint {
    const KIND: EndpointKind = EndpointKind::Setpoint;
    type Client<E: Endpoint> = Publish<E>;
    type Owner<E: Endpoint> = Subscribe<E>;
}

impl sealed::Semantics for Stream<In> {}
impl EndpointSemantics for Stream<In> {
    const KIND: EndpointKind = EndpointKind::Stream;
    type Client<E: Endpoint> = Publish<E>;
    type Owner<E: Endpoint> = Subscribe<E>;
}
impl StreamDelivered for Stream<In> {}

impl sealed::Semantics for Stream<Out> {}
impl EndpointSemantics for Stream<Out> {
    const KIND: EndpointKind = EndpointKind::Stream;
    type Client<E: Endpoint> = Subscribe<E>;
    type Owner<E: Endpoint> = Publish<E>;
}
impl StreamDelivered for Stream<Out> {}

impl sealed::Semantics for Query {}
impl EndpointSemantics for Query {
    const KIND: EndpointKind = EndpointKind::Query;
    type Client<E: Endpoint> = AskQuery<E>;
    type Owner<E: Endpoint> = ServeQuery<E>;
}

impl sealed::Semantics for WorldClock {}
impl EndpointSemantics for WorldClock {
    // The wire kind is unchanged from the ordinary event it has always been;
    // only the Rust-level authority differs.
    const KIND: EndpointKind = EndpointKind::Event;
    type Client<E: Endpoint> = Subscribe<E>;
    type Owner<E: Endpoint> = Publish<E>;
}
impl StreamDelivered for WorldClock {}

/// One endpoint: the payload (or request) type it carries, plus the family and
/// semantics attached to it by its own `endpoints!` declaration.
///
/// The trait is sealed. A payload genuinely carried by two independent
/// endpoints needs two meaningful newtypes, not one type with two contracts.
pub trait Endpoint:
    Payload + crate::__compat::wire::DescribeWire + sealed::Endpoint + 'static
{
    /// The wire family this endpoint belongs to.
    type Family: Family;
    /// What the endpoint means, and therefore how it may be operated.
    type Semantics: EndpointSemantics;
}

/// A [`Query`] endpoint: the request type, plus the response it is answered
/// with.
pub trait QueryEndpoint: Endpoint<Semantics = Query> {
    /// The response payload encoded in the reply.
    type Response: Payload + crate::__compat::wire::DescribeWire;
}

/// An endpoint of the [`Robot`] family: what participant IO accepts.
///
/// Blanket-implemented, so it is a name for the family bound rather than a
/// second thing a declaration has to say. `RuntimeEndpoint` and
/// `SupervisorEndpoint` deliberately do not exist: no bound needs them.
pub trait RobotEndpoint: Endpoint<Family = Robot> {}

impl<E: Endpoint<Family = Robot>> RobotEndpoint for E {}

#[cfg(test)]
mod tests {
    use super::*;

    /// Each semantic's delivery lane follows from its wire kind, so a
    /// declaration cannot state a kind and a lane that disagree.
    #[test]
    fn every_semantic_derives_its_lane_from_its_kind() {
        for (kind, delivery) in [
            (
                <State as EndpointSemantics>::KIND,
                <State as EndpointSemantics>::DELIVERY,
            ),
            (
                <Sample as EndpointSemantics>::KIND,
                <Sample as EndpointSemantics>::DELIVERY,
            ),
            (
                <Event as EndpointSemantics>::KIND,
                <Event as EndpointSemantics>::DELIVERY,
            ),
            (
                <Setpoint as EndpointSemantics>::KIND,
                <Setpoint as EndpointSemantics>::DELIVERY,
            ),
            (
                <Stream<In> as EndpointSemantics>::KIND,
                <Stream<In> as EndpointSemantics>::DELIVERY,
            ),
            (
                <Stream<Out> as EndpointSemantics>::KIND,
                <Stream<Out> as EndpointSemantics>::DELIVERY,
            ),
            (
                <Query as EndpointSemantics>::KIND,
                <Query as EndpointSemantics>::DELIVERY,
            ),
            (
                <WorldClock as EndpointSemantics>::KIND,
                <WorldClock as EndpointSemantics>::DELIVERY,
            ),
        ] {
            assert_eq!(kind.delivery_family(), delivery);
        }
    }

    /// The world clock keeps the wire shape of the event it has always been:
    /// its distinctness is a Rust-level authority, not a wire change.
    #[test]
    fn the_world_clock_keeps_the_event_wire_kind() {
        assert_eq!(
            <WorldClock as EndpointSemantics>::KIND,
            <Event as EndpointSemantics>::KIND
        );
        assert_eq!(
            <WorldClock as EndpointSemantics>::DELIVERY,
            DeliveryFamily::Stream
        );
    }

    /// Every family is rooted at its own name, which is the leading segment of
    /// every key below it.
    #[test]
    fn every_family_identifies_itself_by_name() {
        assert_eq!(<Robot as Family>::ID, "robot");
        assert_eq!(<Runtime as Family>::ID, "runtime");
        assert_eq!(<Supervisor as Family>::ID, "supervisor");
    }
}
