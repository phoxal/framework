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
/// [`ApiVersion`] and one contract family/topic (D61).
///
/// Every body declared inside a `phoxal_api_tree!` node gets a generated impl.
/// Handles, `SetupContext` builders, and the `Service`/`Driver` derive assertions
/// all key off [`Api`](ContractBody::Api)/[`FAMILY`](ContractBody::FAMILY)/[`TOPIC`](ContractBody::TOPIC),
/// which is how a body from the wrong API version is rejected at compile time.
///
/// The serde encoding of an implementor *is* the wire payload; there is no version
/// envelope (D62). Version and family travel as bus metadata, derived from
/// [`Api`](ContractBody::Api) and [`FAMILY`](ContractBody::FAMILY).
///
/// Runtime decode compatibility keys off [`SCHEMA_ID`](ContractBody::SCHEMA_ID),
/// not the graph API version. `SCHEMA_ID` is a normalized hash of the body's
/// transitive wire shape. The canonical hash input is `phoxal.schema/v0\n`
/// followed by this grammar:
///
/// ```text
/// body    = unit | newtype | tuple | struct | enum
/// struct  = "struct{" field* "}"
/// field   = "f" byte_len ":" utf8_name "=" ty ";"
/// enum    = "enum(" repr "){" variant* "}"
/// repr    = "external" | "internal(tag=" name ")"
///         | "adjacent(tag=" name ",content=" name ")" | "untagged"
/// variant = "v" byte_len ":" utf8_name "=" payload ";"
/// payload = unit | "newtype(" ty ")" | tuple | struct
/// newtype = "newtype(" ty ")"
/// tuple   = "tuple(" (ty ";")* ")"
/// ty      = nil | bool | string | bytes | integer | float | option | seq | array
///         | map | tuple | body
/// integer = "u8" | "u16" | "u32" | "u64" | "u128"
///         | "i8" | "i16" | "i32" | "i64" | "i128"
/// float   = "f32" | "f64"
/// option  = "option(" ty ")"
/// seq     = "seq(" ty ")"
/// array   = "array[" decimal_len "](" ty ")"
/// map     = "map(" ty "," ty ")"
/// name    = byte_len ":" utf8_name
/// ```
///
/// Fields stay in declared order and use their serde wire names, including
/// `rename` and `rename_all`. Enums include serde representation mode, variant
/// wire names, and payload shapes. Nested API-tree structs and enums are expanded
/// transitively, with generic type parameters substituted by concrete type
/// arguments before expansion. Only understood serde attributes and known
/// wire-neutral attributes such as doc comments and simple derives are accepted.
/// Non-wire details such as Rust type identifiers when the wire shape has already
/// been expanded are excluded. The displayed id is lower-hex
/// `sha256(canonical_string)`, truncated to the first 16 hex characters.
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
    /// Per-contract compatibility id: lower-hex SHA-256 of the canonical
    /// transitive wire-shape string, truncated to 16 hex characters.
    const SCHEMA_ID: &'static str;
    /// Versionless topic key: the `/`-joined node path plus the topic leaf, with
    /// each dynamic node contributing a `{var}` placeholder, e.g. `"drive/state"`
    /// or `"component/{instance}/motor/{capability}/command"`. The concrete key is
    /// produced by the api-local `topic` builder, which fills the placeholders.
    const TOPIC: &'static str;
}
