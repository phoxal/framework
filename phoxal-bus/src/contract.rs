//! The contract primitive traits (D60/D61): the API-version marker and the
//! version-local wire body.
//!
//! These are the two traits the bus client is generic over - the ABI floor every
//! contract body and api-version marker implements. The concrete dated API
//! versions (`phoxal::api::y2026_1`, …) and the `phoxal_api_tree!` macro that
//! generates their `ApiVersion` / `ContractBody` impls live in the `phoxal`
//! engine crate, which re-exports these traits at `phoxal::api::ApiVersion` /
//! `phoxal::api::ContractBody`.

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
