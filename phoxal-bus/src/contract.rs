//! The contract primitive traits (D60/D61): the API-version marker and the
//! version-local wire body.
//!
//! These are the two traits the bus client is generic over - the ABI floor every
//! contract body and api-version marker implements. The concrete dated API
//! versions (`phoxal::api::y2026_1`, …) and the `phoxal_api_tree!` macro that
//! generates their `ApiVersion` / `ContractBody` impls live in the `phoxal-api`
//! crate, which the `phoxal` engine re-exports at `phoxal::api` - so these traits
//! are reachable as `phoxal::api::ApiVersion` / `phoxal::api::ContractBody`.

/// Marker trait identifying one dated API version (D60).
///
/// Implemented only by the zero-variant `enum Api {}` that
/// [`phoxal_api_tree!`] generates inside each version module. The [`ID`] is the
/// dated module name (`"y2026_1"`) and is the canonical version identity: it is
/// carried in bus metadata, never in the wire body or the topic key (D62).
///
/// [`ID`]: ApiVersion::ID
/// [`phoxal_api_tree!`]: https://docs.rs/phoxal
pub trait ApiVersion: 'static {
    /// The dated API-version identifier, equal to the version module name, e.g.
    /// `"y2026_1"`.
    const ID: &'static str;
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
/// [`ApiVersion`] and one contract family/topic (D61).
///
/// Every body declared inside a `phoxal_api_tree!` node gets a generated impl.
/// Handles, `SetupContext` builders, and the `#[derive(Runtime)]` assertions all
/// key off [`Api`](ContractBody::Api)/[`FAMILY`](ContractBody::FAMILY)/[`TOPIC`](ContractBody::TOPIC),
/// which is how a body from the wrong API version is rejected at compile time.
///
/// The serde encoding of an implementor *is* the wire payload; there is no version
/// envelope (D62). Version and family travel as bus metadata, derived from
/// [`Api`](ContractBody::Api) and [`FAMILY`](ContractBody::FAMILY).
pub trait ContractBody:
    serde::Serialize + serde::de::DeserializeOwned + Clone + Send + Sync + 'static
{
    /// The single API version this body belongs to. Two bodies from different
    /// versions have different `Api`, so the type system keeps them apart.
    type Api: ApiVersion;
    /// Canonical contract family id: the `::`-joined node path plus the body type
    /// name, with dynamic variables excluded, e.g. `"drive::State"` or
    /// `"component::motor::Command"`.
    const FAMILY: &'static str;
    /// Versionless topic key: the `/`-joined node path plus the topic leaf, with
    /// each dynamic node contributing a `{var}` placeholder, e.g. `"drive/state"`
    /// or `"component/{instance}/motor/{capability}/command"`. The runtime key is
    /// produced by the api-local `topic` builder, which fills the placeholders.
    const TOPIC: &'static str;
}
