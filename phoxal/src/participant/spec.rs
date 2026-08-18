//! Static participant identity and associated authoring types.

use super::config::ParticipantConfig;

/// Static participant identity and associated types, emitted by a role
/// attribute on a unit marker.
#[doc(hidden)]
pub trait ParticipantSpec: Sized + Send + Sync + 'static {
    /// The authoring kind that produced this artifact, as the framework-owned
    /// value the embedded metadata record declares.
    const KIND: crate::participant::metadata::ParticipantKind;
    /// The participant id (`id = "…"`, default derived from the crate's
    /// `CARGO_PKG_NAME`; see `#[phoxal::service]`'s docs).
    const ID: &'static str;
    /// The single contract family every typed handle this participant builds
    /// must come from. Role attributes fix it to the participant-authoring
    /// facade (`phoxal::api::Api`, the `robot` family); there is no
    /// participant-local choice.
    ///
    /// Every [`SetupContext`](crate::SetupContext) builder is bounded on it, so
    /// one participant physically cannot construct handles from two contract
    /// families.
    #[doc(hidden)]
    type ContractApi: crate::bus::ApiFamily;
    /// The participant's typed config (`robot.yaml` input).
    type Config: ParticipantConfig;
    /// Mutable runtime state, owned only by the serialized event loop.
    type State: Send + 'static;
    /// Bus-facing handles used by lifecycle behavior.
    type Api: Send + 'static;

    /// Construct the role marker. Role attributes accept unit structs only.
    #[doc(hidden)]
    fn __new() -> Self;

    /// Read a byte out of this participant's embedded `.phoxal_meta` /
    /// `__DATA,__phoxal_meta` metadata static, so it is reachable from the
    /// process entry point.
    ///
    /// On ELF (Linux), `--gc-sections` drops any section unreachable from
    /// `main` at final link time - even one carrying `#[used]`, which only
    /// protects the static from this compilation unit's own dead-code
    /// elimination. `phoxal::run`/`run_async` call this once before doing
    /// anything else so the static is genuinely reachable.
    #[doc(hidden)]
    fn __retain_embedded_metadata();
}
