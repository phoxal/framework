//! The contract primitive traits (D60/D61): the API-version marker and the
//! version-local wire body.
//!
//! These are the two traits the bus client is generic over - the ABI floor every
//! contract body and api-version marker implements. The concrete dated API
//! versions (`phoxal_api::y2026_1`, …) and the `phoxal_api_tree!` macro that
//! generates their `ApiVersion` / `ContractBody` impls live in the `phoxal-api`
//! crate, which re-exports these traits - so they are reachable as
//! `phoxal_api::ApiVersion` / `phoxal_api::ContractBody`. The `phoxal` engine
//! re-exports this bus crate at `phoxal::bus`, so they are also reachable as
//! `phoxal::bus::ApiVersion` / `phoxal::bus::ContractBody`.

/// Marker trait identifying one dated API version (D60).
///
/// Implemented only by the zero-variant `enum Api {}` that
/// [`phoxal_api_tree!`] generates inside each version module. The [`ID`] is the
/// dated module name (`"y2026_1"`) and is carried in bus metadata as
/// informational provenance, never in the wire body or the topic key (D62).
/// [`IS_PREVIEW`] records authoring lifecycle only; it has no wire effect.
///
/// [`ID`]: ApiVersion::ID
/// [`IS_PREVIEW`]: ApiVersion::IS_PREVIEW
/// [`phoxal_api_tree!`]: https://docs.rs/phoxal
pub trait ApiVersion: 'static {
    /// The dated API-version identifier, equal to the version module name, e.g.
    /// `"y2026_1"`.
    const ID: &'static str;
    /// Whether this generated API version is still in the preview lifecycle.
    ///
    /// This is control-plane metadata only. It is not encoded in bus payloads,
    /// topics, schema ids, or encoding strings.
    const IS_PREVIEW: bool = false;
}

/// The semantic role a topic plays in its owning service's contract.
///
/// Every topic in a [`phoxal_api_tree!`] declares one of these (D63, plan #00).
/// The role records *intent*, separate from the wire shape: a `Command` and a
/// `State` topic are both pub/sub on the wire, but the owner subscribes a
/// `Command` (it is the service's control input) and publishes a `State` (it is
/// the service's telemetry output). `Query` is the request/response role.
///
/// The role drives the side branding (L1): the api tree's builders read it to pick
/// each leaf's side-branded topic kind (`Publish`/`Subscribe`/`AskQuery`/`ServeQuery`),
/// so taking the wrong side of a topic is a compile error. The role also rides
/// alongside each generated contract body as a `ROLE` const; that const is not yet
/// emitted by `emit-apis` (a later increment of plan #00).
///
/// [`phoxal_api_tree!`]: https://docs.rs/phoxal
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum TopicRole {
    /// A control input the owning service subscribes (e.g. `drive/target`).
    Command,
    /// A telemetry/output the owning service publishes (e.g. `drive/state`).
    State,
    /// A request/response topic the owning service answers (e.g. `map/submap`).
    Query,
}

impl TopicRole {
    /// The lowercase grammar keyword for this role (`"command"` / `"state"` /
    /// `"query"`), matching how it is written in `phoxal_api_tree!`.
    pub const fn as_str(self) -> &'static str {
        match self {
            TopicRole::Command => "command",
            TopicRole::State => "state",
            TopicRole::Query => "query",
        }
    }
}

/// A version-local wire body: a plain serde type bound to exactly one
/// [`ApiVersion`] and one contract topic (D61/D1).
///
/// Every body declared inside a `phoxal_api_tree!` node gets a generated impl.
/// Handles, `SetupContext` builders, and the `Service`/`Driver` derive assertions
/// all key off [`Api`](ContractBody::Api)/[`TOPIC`](ContractBody::TOPIC), which is
/// how a body from the wrong API version is rejected at compile time.
///
/// The serde encoding of an implementor *is* the wire payload; there is no version
/// envelope (D62).
///
/// **Wire identity is the key, not a hash (D1).** The generation is folded into
/// [`TOPIC`](ContractBody::TOPIC), so `y2026_1::drive::Target` and a
/// hypothetically re-minted `y2026_2::drive::Target` publish on different Zenoh
/// keys and physically cannot collide. There is therefore no `SCHEMA_ID`/`FAMILY`
/// axis: two participants interoperate on a contract iff they use the exact same
/// version-qualified name, which is enforced by the type system (`Api` bound) and
/// realized on the wire by the key. A receiver's per-key Zenoh subscription is the
/// whole fast-reject; the bus decode path validates only the codec.
pub trait ContractBody:
    serde::Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + 'static
{
    /// The single API version this body belongs to. Two bodies from different
    /// versions have different `Api`, so the type system keeps them apart.
    type Api: ApiVersion;
    /// The version-qualified type path this body's own generation module
    /// places it at: the version module name, then the `::`-joined node path
    /// (dynamic-node vars are topic params, never type-path segments), then
    /// the PascalCase type leaf, e.g. `"y2026_1::drive::Target"` or
    /// `"y2026_1::component::motor::Command"`. This is the contract's source
    /// identity (D1) - two contracts interoperate iff they share this exact
    /// name - as distinct from [`TOPIC`](ContractBody::TOPIC), the resolved
    /// wire key derived from it.
    const NAME: &'static str;
    /// The generation-qualified wire key: the version module name, then the
    /// `/`-joined node path plus the topic leaf, with each dynamic node
    /// contributing a `{var}` placeholder, e.g. `"y2026_1/drive/state"` or
    /// `"y2026_1/component/{instance}/motor/{capability}/command"`. The concrete
    /// key is produced by the api-local `topic` builder, which fills the
    /// placeholders.
    const TOPIC: &'static str;
}
