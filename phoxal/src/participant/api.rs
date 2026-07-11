//! The participant authoring model (`Api`/`Config`/`Participant`).
//! `#[derive(phoxal::Api)]` / `#[derive(phoxal::Config)]` /
//! `#[phoxal::service|driver|simulator|tool]` / `#[phoxal::behavior]` target
//! the traits here.
//!
//! `Api` names a participant-authored struct of bus handles, and a participant
//! may mix contract generations freely across its `Api` fields - there is no
//! per-participant API version ceiling.
//!
//! The runner's own system contracts (heartbeat/presence/simulation clock) do
//! not resolve a version through this trait hierarchy either:
//! `participant::runner` hardcodes `use phoxal_api::y2026_1 as api;`,
//! independent of any participant's chosen `Api`.
//!
//! # What this slice defers
//!
//! - **Deferred hardening for `#[server_snapshot]`.** The generated
//!   [`ParticipantLifecycle::__serve_snapshot`] takes `Arc<Self::Api>`
//!   (read-only, D3): the runner constructs that `Arc` (one clone of the
//!   `#[setup]`-returned `api`) and hands `Arc::clone`s to each spawned
//!   `#[server_snapshot]` task on a live bus, alongside the owned `api` the
//!   main task keeps for `&mut Self::Api`. `Arc<Self::Api>` (not
//!   `&Self::Api`) is the chosen "api snapshot" shape (D3 offers either):
//!   every bus handle type's real operations already take `&self` (see
//!   `phoxal-bus/src/handle.rs`), so an `Arc` costs nothing extra and - unlike
//!   a borrowed reference - is `'static` and can be moved into the
//!   spawned/boxed future `__serve_snapshot` returns. This is sound for
//!   `Publisher`/`Latest`/`Querier`/`Server` (non-destructive reads / fresh
//!   publishes); a `Subscriber` field, however, is a *destructive* shared
//!   queue, so a `#[server_snapshot]` handler must read committed `Snapshot`
//!   state and never `recv` a `Subscriber` (see `runner`'s module docs and
//!   `Subscriber`'s rustdoc). **Deferred hardening:** this slice does not yet
//!   *enforce* that structurally - rejecting a snapshot handler that drains a
//!   `Subscriber` at compile time would need an invasive `Api`-projection
//!   redesign (a separate read-only snapshot view type excluding `Subscriber`
//!   fields); until it lands, P-convert must uphold the rule per participant.
//!
//! The owner-capability / component / raw-bus setup accessors are **not**
//! deferred: the surface exposes `owner_capability()` for all participants,
//! `component()` for drivers and simulators, and `raw_bus()` for tools (see
//! [`SetupContextApiExt`], [`SetupContextDriverExt`],
//! [`SetupContextSimulatorExt`], [`SetupContextToolExt`] below).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use crate::bus::{
    AskQuery, ContractBody, DEFAULT_QUERY_TIMEOUT, Latest, OwnerCap, Publish, Publisher, Querier,
    Subscribe, Subscriber, Topic,
};
use crate::participant::context::{SetupContext, ShutdownContext, StepContext};
use crate::participant::server::ServerOutcome;
use crate::participant::spec::{IsDriver, IsSimulator, IsTool, StepSchedule};
use phoxal_bus::Bus;

/// Const-eval plumbing `#[derive(phoxal::Api)]` (`phoxal-macros/src/authoring.rs`)
/// uses to embed a **resolved, version-qualified** contract identity in the
/// linker-section metadata static, rather than the source path as written.
///
/// The problem this solves: every participant writes
/// `use phoxal_api::y2026_1 as api;`, so a macro-time string literal of a
/// field's body type as written (`api::drive::Target`) has the generation
/// erased - it cannot tell a `y2026_1` contract from a same-named `y2026_7`
/// one. The generation-qualified identity *is* available, but only as
/// `<Body as ContractBody>::NAME` (`phoxal-bus/src/contract.rs`), an
/// associated const on a foreign type the proc-macro cannot evaluate at
/// expansion time - only `rustc`, during the downstream participant crate's
/// own const-eval, can resolve it. So the derive emits **tokens**, not a
/// string: a call into [`__concatcp`] splicing `<Body as
/// ContractBody>::NAME` between macro-time-known JSON literal fragments
/// (field name, role), which `rustc` const-evaluates in the participant
/// crate. The result is a `&'static str`, but the linker section is read as
/// raw bytes by xtask's `object`-crate extractor (`xtask/src/release/metadata.rs`),
/// so [`__bytes_of`] copies that resolved string into a fixed-size `[u8; N]`
/// const array - `N` inferred from the manifest const's own `.len()` at the
/// static's declaration site - which is what actually gets placed in the
/// `#[link_section]`.
#[doc(hidden)]
pub mod __meta {
    /// Re-exported so `#[derive(phoxal::Api)]`'s generated code can reach it
    /// as `phoxal::participant::api::__meta::__concatcp!(..)` without every
    /// participant crate needing its own `const_format` dependency.
    /// `concatcp!` (unlike `std::concat!`) accepts constant *expressions*,
    /// not just literals - in particular, a path to a foreign associated
    /// const like `<Body as ContractBody>::NAME` - which is exactly the
    /// piece a proc-macro cannot pre-resolve into a literal.
    pub use const_format::concatcp as __concatcp;

    /// Fixed-capacity const-eval string builder used for recursively composed
    /// config schemas. A fixed backing array is necessary because stable Rust
    /// cannot express an array length computed from a generic associated
    /// constant; only the used prefix is exposed by [`ConstSchema::as_str`].
    #[derive(Clone, Copy)]
    pub struct ConstSchema {
        bytes: [u8; 65_536],
        len: usize,
    }

    impl ConstSchema {
        pub const fn new() -> Self {
            Self {
                bytes: [0; 65_536],
                len: 0,
            }
        }

        pub const fn from_str(value: &str) -> Self {
            Self::new().push_str(value)
        }

        #[must_use]
        pub const fn push_str(mut self, value: &str) -> Self {
            let value = value.as_bytes();
            assert!(
                self.len + value.len() <= self.bytes.len(),
                "phoxal: const config schema exceeds 64 KiB"
            );
            let mut index = 0;
            while index < value.len() {
                self.bytes[self.len + index] = value[index];
                index += 1;
            }
            self.len += value.len();
            self
        }

        pub const fn as_str(&self) -> &str {
            let (used, _) = self.bytes.split_at(self.len);
            // Every byte originates in a Rust `&str`, so concatenation
            // preserves UTF-8 validity.
            unsafe { core::str::from_utf8_unchecked(used) }
        }
    }

    /// Copies a `rustc`-const-evaluated `&str` into a fixed `[u8; N]` array so
    /// it can be assigned to a `#[link_section]` static (which must be a
    /// plain byte-sized value, not a fat `&str` pointer/len pair). Callers
    /// are expected to pass `N` implicitly via the assignment's expected
    /// array type (`static X: [u8; LEN] = __bytes_of(S);` with `const LEN:
    /// usize = S.len();`); the `assert!` is a defense-in-depth check against
    /// that inference ever mismatching, not the primary correctness
    /// mechanism.
    pub const fn __bytes_of<const N: usize>(s: &str) -> [u8; N] {
        let bytes = s.as_bytes();
        assert!(
            bytes.len() == N,
            "phoxal: metadata manifest length mismatch"
        );
        let mut out = [0u8; N];
        let mut i = 0;
        while i < N {
            out[i] = bytes[i];
            i += 1;
        }
        out
    }
}

/// The role a [`ParticipantApi`] handle field plays on the bus.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ContractRole {
    /// A `Publisher<B>` field (or `Vec`/map of one).
    Publish,
    /// A `Subscriber<B>`/`Latest<B>` field (or `Vec`/map of one).
    Subscribe,
    /// A `Server<Req, Resp>` field (or `Vec`/map of one); contributes one
    /// entry for `Req` and one for `Resp`.
    Serve,
    /// A `Querier<Req, Resp>` field (or `Vec`/map of one) - the CLIENT/asking
    /// side of a query contract (the counterpart to [`Serve`](Self::Serve));
    /// contributes one entry for `Req` and one for `Resp`.
    Ask,
}

/// One contract a `Api` struct field uses: its generation-qualified wire key
/// (D1) plus the role that field plays. Built by `#[derive(phoxal::Api)]` from
/// each field's `<Body as ContractBody>::TOPIC`.
#[derive(Clone, Copy, Debug)]
pub struct ApiContractUse {
    /// The generation-qualified wire key.
    pub topic: &'static str,
    /// The role this field plays for that contract.
    pub role: ContractRole,
}

/// Emitted by `#[derive(phoxal::Api)]`: the bus-facing contract surface of a
/// participant's `Api` handle struct.
///
/// `Clone`: every bus handle type (`Publisher`/`Latest`/`Subscriber`/
/// `Querier`/[`Server`]) is cheaply `Clone` - a second handle to the same
/// underlying subscription/publish key/session, never a deep copy - because
/// every real operation on them takes `&self` (`phoxal-bus/src/handle.rs`'s
/// module docs). `#[derive(phoxal::Api)]` emits a field-wise `Clone` impl
/// alongside the `ParticipantApi` impl, so this bound is satisfied
/// automatically for every derived `Api` struct. The runner
/// (`participant::runner`) uses it to give concurrent `#[server_snapshot]`
/// tasks their own `Arc<Self::Api>` - a full clone made once after
/// `#[setup]`, independent of the `&mut Self::Api` the main task keeps for
/// `#[step]`/`#[server]`/`#[shutdown]` - rather than one value shared behind
/// `&mut`/`Arc` at once, which Rust's aliasing rules forbid without unsafe
/// code. Because every clone is a handle to the same live state (shared
/// `Bus`/`ArcSwapOption`/subscription task), the two never diverge, so this
/// is exactly D3's "read-only `&Self::Api`, or an api snapshot" - here
/// realized as a cloned api snapshot.
pub trait ParticipantApi: Send + Sync + Clone + 'static {
    #[doc(hidden)]
    const __NAME: &'static str;
    #[doc(hidden)]
    const __CONTRACTS_JSON: &'static str;
    /// Every contract this `Api` struct's fields use, deduplicated.
    const CONTRACTS: &'static [ApiContractUse];
}

/// `Api = ()` for participants that opt out of a typed bus surface (tools,
/// per decision - "Tools stay raw-bus only", `remove-emit-apis-api-authoring/readme.md`).
impl ParticipantApi for () {
    const __NAME: &'static str = "()";
    const __CONTRACTS_JSON: &'static str = "[]";
    const CONTRACTS: &'static [ApiContractUse] = &[];
}

/// Per-`Api`-struct marker: this `Api` declared a *publish* handle for body
/// `B` (D44). Emitted by `#[derive(phoxal::Api)]` for each `Publisher<B>`
/// field (including `Vec`/`BTreeMap`/`HashMap` of one). [`SetupContextApiExt::publisher`]
/// carries `where R::Api: DeclaresPublish<B>`, so building a publisher for a
/// family the `Api` struct never declared as a field is a compile error -
/// this is what makes [`ParticipantApi::CONTRACTS`] a guaranteed-complete
/// picture of the participant's bus surface, not just a lower bound (see the
/// trait's docs).
pub trait DeclaresPublish<B: ?Sized> {}

/// Per-`Api`-struct marker: this `Api` declared a *subscribe* handle for body
/// `B` (`Subscriber<B>`/`Latest<B>` fields, including `Vec`/`BTreeMap`/`HashMap`
/// of one). See [`DeclaresPublish`]; [`SetupContextApiExt::latest`] and
/// [`SetupContextApiExt::subscriber`] both carry `where R::Api: DeclaresSubscribe<B>`.
pub trait DeclaresSubscribe<B: ?Sized> {}

/// Per-`Api`-struct marker: this `Api` declared a *query* (asking/client)
/// handle for `Req`/`Resp` (`Querier<Req, Resp>` fields). See
/// [`DeclaresPublish`]; [`SetupContextApiExt::querier`] carries
/// `where R::Api: DeclaresAsk<Req, Resp>`.
pub trait DeclaresAsk<Req: ?Sized, Resp: ?Sized> {}

/// Per-`Api`-struct marker: this `Api` declared a *serve* (answering/server)
/// handle for `Req`/`Resp` (`Server<Req, Resp>` fields). See
/// [`DeclaresPublish`]; [`SetupContextApiExt::server`] carries
/// `where R::Api: DeclaresServe<Req, Resp>`.
pub trait DeclaresServe<Req: ?Sized, Resp: ?Sized> {}

/// Emitted by `#[derive(phoxal::Config)]`: the participant config's compile-time
/// JSON Schema (Draft 2020-12).
pub trait ParticipantConfig: serde::de::DeserializeOwned + Send + 'static {
    #[doc(hidden)]
    const __SCHEMA: __meta::ConstSchema;
    /// A complete schema or subschema, const-composable by another derived
    /// config without runtime allocation.
    const SCHEMA_JSON: &'static str = Self::__SCHEMA.as_str();
}

impl ParticipantConfig for () {
    const __SCHEMA: __meta::ConstSchema = __meta::ConstSchema::from_str(r#"{"type":"null"}"#);
}

/// An optional config is a config: `config = Option<T>` (a participant whose
/// `PHOXAL_CONFIG` may be absent, deserializing to `None`) works whenever
/// `T: ParticipantConfig`; `Option<T>`'s own `Deserialize` maps `null`/absent
/// to `None` and a present object to `Some(T)`. A participant crate cannot
/// write `impl ParticipantConfig for Option<LocalConfig>` itself (orphan
/// rule: both the trait and `Option` are foreign), so the blanket lives here.
impl<T: ParticipantConfig> ParticipantConfig for Option<T> {
    const __SCHEMA: __meta::ConstSchema = __meta::ConstSchema::new()
        .push_str(r#"{"anyOf":["#)
        .push_str(T::SCHEMA_JSON)
        .push_str(r#",{"type":"null"}]}"#);
}

impl<T: ParticipantConfig> ParticipantConfig for Vec<T> {
    const __SCHEMA: __meta::ConstSchema = __meta::ConstSchema::new()
        .push_str(r#"{"type":"array","items":"#)
        .push_str(T::SCHEMA_JSON)
        .push_str("}");
}

impl<T: ParticipantConfig> ParticipantConfig for std::collections::BTreeMap<String, T> {
    const __SCHEMA: __meta::ConstSchema = __meta::ConstSchema::new()
        .push_str(r#"{"type":"object","additionalProperties":"#)
        .push_str(T::SCHEMA_JSON)
        .push_str("}");
}

impl<T: ParticipantConfig> ParticipantConfig for std::collections::HashMap<String, T> {
    const __SCHEMA: __meta::ConstSchema = __meta::ConstSchema::new()
        .push_str(r#"{"type":"object","additionalProperties":"#)
        .push_str(T::SCHEMA_JSON)
        .push_str("}");
}

macro_rules! primitive_config_schema {
    ($ty:ty => $schema:literal) => {
        impl ParticipantConfig for $ty {
            const __SCHEMA: __meta::ConstSchema = __meta::ConstSchema::from_str($schema);
        }
    };
}

primitive_config_schema!(bool => r#"{"type":"boolean"}"#);
primitive_config_schema!(String => r#"{"type":"string"}"#);
primitive_config_schema!(char => r#"{"type":"string","minLength":1,"maxLength":1}"#);
primitive_config_schema!(i8 => r#"{"type":"integer","format":"int8","minimum":-128,"maximum":127}"#);
primitive_config_schema!(i16 => r#"{"type":"integer","format":"int16","minimum":-32768,"maximum":32767}"#);
primitive_config_schema!(i32 => r#"{"type":"integer","format":"int32","minimum":-2147483648,"maximum":2147483647}"#);
primitive_config_schema!(i64 => r#"{"type":"integer","format":"int64"}"#);
primitive_config_schema!(i128 => r#"{"type":"integer"}"#);
primitive_config_schema!(isize => r#"{"type":"integer"}"#);
primitive_config_schema!(u8 => r#"{"type":"integer","format":"uint8","minimum":0,"maximum":255}"#);
primitive_config_schema!(u16 => r#"{"type":"integer","format":"uint16","minimum":0,"maximum":65535}"#);
primitive_config_schema!(u32 => r#"{"type":"integer","format":"uint32","minimum":0,"maximum":4294967295}"#);
primitive_config_schema!(u64 => r#"{"type":"integer","format":"uint64","minimum":0}"#);
primitive_config_schema!(u128 => r#"{"type":"integer","minimum":0}"#);
primitive_config_schema!(usize => r#"{"type":"integer","minimum":0}"#);
primitive_config_schema!(f32 => r#"{"type":"number","format":"float"}"#);
primitive_config_schema!(f64 => r#"{"type":"number","format":"double"}"#);

/// Emitted by `#[phoxal::service]` / `#[phoxal::driver]` /
/// `#[phoxal::simulator]` / `#[phoxal::tool]`: participant identity plus the
/// linked `Config`/`Api` types.
pub trait Participant: Sized + Send + 'static {
    /// The authoring kind that produced this artifact (`"service"`,
    /// `"driver"`, `"simulator"`, or `"tool"`).
    const KIND: &'static str;
    /// Whether normal graph topology applies to this participant.
    const PARTICIPANT_CLASS: &'static str;
    /// The participant id (`id = "…"`, default kebab of the type name).
    const ID: &'static str;
    /// The participant's typed config (`robot.yaml` input).
    type Config: ParticipantConfig;
    /// The participant's bus-facing contract surface (`()` for a raw-bus
    /// tool).
    type Api: ParticipantApi;
}

/// Lifecycle dispatch + server-side metadata, emitted by `#[phoxal::behavior]`
/// for a `#[setup]` returning `Result<(Self, Self::Api)>`, threading
/// `Self::Api` through every callback (D3):
///
/// - `#[step]` / the exclusive `#[server(api = …)]` get `&mut Self::Api`
///   (same task as the caller, so exclusive access is free);
/// - the concurrent `#[server_snapshot(api = …)]` gets a shared
///   `Arc<Self::Api>`, not `&mut` - it may run concurrently with `#[step]`/an
///   exclusive server (D3's "read-only … or an api snapshot"; see the module
///   docs for why `Arc` is this slice's chosen shape).
#[allow(async_fn_in_trait)]
pub trait ParticipantLifecycle: Participant {
    /// Contracts derived from `#[server]`/`#[server_snapshot]` handler
    /// signatures.
    const SERVER_CONTRACTS: &'static [ApiContractUse];

    /// The committed-snapshot state type (`()` when there is no
    /// `#[snapshot]`).
    type Snapshot: Send + Sync + 'static;

    /// Whether the participant provides a committed snapshot (`#[snapshot]`).
    const HAS_SNAPSHOT: bool;

    /// The versionless topic keys of exclusive `#[server]` handlers.
    fn __exclusive_server_topics() -> &'static [&'static str];

    /// The versionless topic keys of concurrent `#[server_snapshot]`
    /// handlers.
    fn __snapshot_server_topics() -> &'static [&'static str];

    /// Reject a duplicate server topic before startup declares queryables.
    fn __validate_server_topics() -> Result<(), String>;

    /// The scheduled-step cadence, or `None` if the participant has no
    /// `#[step]`.
    fn __step_schedule() -> Option<StepSchedule>;

    /// Construct the participant and its `Api` (`#[setup]`).
    async fn __setup(
        ctx: &mut SetupContext<Self>,
        config: Self::Config,
    ) -> crate::Result<(Self, Self::Api)>;

    /// Run one scheduled step (`#[step]`; a no-op when none is declared).
    async fn __step(&mut self, api: &mut Self::Api, step: StepContext) -> crate::Result<()>;

    /// Graceful shutdown (`#[shutdown]`; a no-op when none is declared).
    async fn __shutdown(&mut self, api: &mut Self::Api, ctx: ShutdownContext) -> crate::Result<()>;

    /// Commit the current state as a snapshot (calls the `#[snapshot]`
    /// provider; returns `()` when there is none).
    fn __take_snapshot(&self) -> Self::Snapshot;

    /// Serve one exclusive `#[server]` query (holds `&mut self` and
    /// `&mut Self::Api`, serialized with `#[step]`).
    async fn __serve_exclusive(
        &mut self,
        api: &mut Self::Api,
        topic: &str,
        request: &[u8],
    ) -> ServerOutcome;

    /// Serve one concurrent `#[server_snapshot]` query against a committed
    /// state snapshot and a shared, read-only `Api` snapshot (D3). Returns a
    /// boxed `Send` future so the caller can spawn it.
    fn __serve_snapshot(
        snapshot: Arc<Self::Snapshot>,
        api: Arc<Self::Api>,
        topic: String,
        request: Vec<u8>,
    ) -> Pin<Box<dyn Future<Output = ServerOutcome> + Send>>;
}

/// A declared server slot in an `Api` struct (`#[derive(phoxal::Api)]`
/// recognizes it as a [`ContractRole::Serve`] contract). Unlike
/// [`Publisher`]/[`Latest`]/[`Subscriber`]/[`Querier`], this carries no live
/// bus connection: serving is runner-dispatched from the generated
/// `ParticipantLifecycle::__serve_*` methods keyed on
/// `<Req as ContractBody>::TOPIC` - the field exists purely so `Api` can
/// *declare* the contract ("`Api` declares the bus contract; `behavior`
/// implements runtime logic").
pub struct Server<Req, Resp> {
    _p: std::marker::PhantomData<fn(Req) -> Resp>,
}

impl<Req, Resp> Server<Req, Resp> {
    /// Framework-internal (macro-only) constructor; the author-facing path is
    /// `ctx.server(...)` in `#[setup]`. `#[doc(hidden)]`.
    #[doc(hidden)]
    pub fn new() -> Self {
        Server {
            _p: std::marker::PhantomData,
        }
    }
}

impl<Req, Resp> Default for Server<Req, Resp> {
    fn default() -> Self {
        Self::new()
    }
}

impl<Req, Resp> Clone for Server<Req, Resp> {
    fn clone(&self) -> Self {
        *self
    }
}

impl<Req, Resp> Copy for Server<Req, Resp> {}

// `PhantomData<fn(Req) -> Resp>` is `Send`/`Sync`/`Copy` regardless of
// `Req`/`Resp` (a fn-pointer phantom, same trick the other handle types'
// `PhantomData<fn() -> B>` markers use), so `Server<Req, Resp>` needs no
// bounds on `Req`/`Resp` to satisfy `ParticipantApi: Send + Sync + Clone`
// (see that trait's docs).

/// `SetupContext<R>` builders (`R: Participant`), added as an extension trait
/// rather than an inherent `impl<R: Participant> SetupContext<R>` block so
/// `context.rs` can stay free of a `Participant` bound on `SetupContext`
/// itself. Bring it into scope with `use phoxal::prelude::*;`.
#[allow(async_fn_in_trait)]
pub trait SetupContextApiExt<R: Participant> {
    /// Build a publisher for `B` (any generation - no per-participant API
    /// version ceiling). `R::Api: DeclaresPublish<B>` (D44): building a
    /// publisher for a contract the `Api` struct did not declare as a
    /// `Publisher<B>` field is a compile error.
    async fn publisher<B: ContractBody>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<Publisher<B>>
    where
        R::Api: DeclaresPublish<B>;

    /// A keep-last-1 view of `B`. `R::Api: DeclaresSubscribe<B>` (D44).
    async fn latest<B: ContractBody>(&self, topic: Topic<Subscribe<B>>) -> crate::Result<Latest<B>>
    where
        R::Api: DeclaresSubscribe<B>;

    /// A drop-oldest ring subscription of `B` at `depth`. `R::Api:
    /// DeclaresSubscribe<B>` (D44).
    async fn subscriber<B: ContractBody>(
        &self,
        topic: Topic<Subscribe<B>>,
        depth: usize,
    ) -> crate::Result<Subscriber<B>>
    where
        R::Api: DeclaresSubscribe<B>;

    /// Build a querier for a declared query contract. `R::Api:
    /// DeclaresAsk<Req, Resp>` (D44).
    async fn querier<Req: ContractBody, Resp: ContractBody>(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Querier<Req, Resp>>
    where
        R::Api: DeclaresAsk<Req, Resp>;

    /// Declare an `Api` server slot for a query contract this participant
    /// serves. See [`Server`] - no live connection is opened here; the
    /// runner dispatches served queries to the generated
    /// `ParticipantLifecycle::__serve_*` methods. `R::Api: DeclaresServe<Req,
    /// Resp>` (D44).
    async fn server<Req: ContractBody, Resp: ContractBody>(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Server<Req, Resp>>
    where
        R::Api: DeclaresServe<Req, Resp>;

    /// The runner-minted owner capability (plan #00 Layer 2) - the controlled
    /// path a participant takes to OWN its own topics. Bind it once in
    /// `#[setup]` and pass it to the owner builder entry
    /// `api::topic::internal::new(cap)`:
    ///
    /// ```ignore
    /// let cap = ctx.owner_capability();
    /// let state = ctx
    ///     .publisher(api::topic::internal::new(cap).drive().state())
    ///     .await?;
    /// ```
    ///
    /// Every real participant starts here, so it is on the base surface (all
    /// `Participant` kinds).
    fn owner_capability(&self) -> OwnerCap;

    /// The resolved robot model (`robot.yaml` + components + structure, D33):
    /// participants build their typed state from it. Present only when the
    /// runner was launched with a robot root; errors otherwise.
    fn robot(&self) -> crate::Result<&crate::model::v0::Robot>;

    /// The robot root directory (holds the robot model + assets). Present only
    /// when launched with a robot root.
    fn robot_root(&self) -> crate::Result<&std::path::Path>;
}

impl<R: Participant> SetupContextApiExt<R> for SetupContext<R> {
    async fn publisher<B: ContractBody>(
        &self,
        topic: Topic<Publish<B>>,
    ) -> crate::Result<Publisher<B>>
    where
        R::Api: DeclaresPublish<B>,
    {
        Ok(Publisher::new(self.bus().clone(), &topic)?)
    }

    async fn latest<B: ContractBody>(&self, topic: Topic<Subscribe<B>>) -> crate::Result<Latest<B>>
    where
        R::Api: DeclaresSubscribe<B>,
    {
        Ok(Latest::new(self.bus(), &topic).await?)
    }

    async fn subscriber<B: ContractBody>(
        &self,
        topic: Topic<Subscribe<B>>,
        depth: usize,
    ) -> crate::Result<Subscriber<B>>
    where
        R::Api: DeclaresSubscribe<B>,
    {
        Ok(Subscriber::new(self.bus(), &topic, depth).await?)
    }

    async fn querier<Req: ContractBody, Resp: ContractBody>(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Querier<Req, Resp>>
    where
        R::Api: DeclaresAsk<Req, Resp>,
    {
        Ok(Querier::new(
            self.bus().clone(),
            &topic,
            DEFAULT_QUERY_TIMEOUT,
        )?)
    }

    async fn server<Req: ContractBody, Resp: ContractBody>(
        &self,
        topic: Topic<AskQuery<Req, Resp>>,
    ) -> crate::Result<Server<Req, Resp>>
    where
        R::Api: DeclaresServe<Req, Resp>,
    {
        // No live bus op: the topic argument only pins `Req`/`Resp` to the
        // declared query contract at the call site (a wrong pairing fails to
        // compile here); dispatch itself is runner-side (see `Server`'s docs).
        let _ = topic;
        Ok(Server::new())
    }

    fn owner_capability(&self) -> OwnerCap {
        self.owner_cap()
    }

    fn robot(&self) -> crate::Result<&crate::model::v0::Robot> {
        self.robot_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no robot model is bound (this participant was launched without a robot root)"
            )
        })
    }

    fn robot_root(&self) -> crate::Result<&std::path::Path> {
        self.robot_root_ref().ok_or_else(|| {
            anyhow::anyhow!(
                "no robot root is bound (this participant was launched without a robot root)"
            )
        })
    }
}

/// Driver-only `SetupContext` accessor (`R: Participant + IsDriver`).
///
/// `component()` lives on a separate extension trait per marker (not a single
/// trait with two blanket impls) precisely because two blanket impls -
/// `IsDriver` and `IsSimulator` - of one trait would overlap under coherence
/// (a type *could* implement both markers, even though none does). Splitting
/// per marker keeps each `ctx.component()` call resolvable to exactly one
/// impl. See also [`SetupContextSimulatorExt`].
pub trait SetupContextDriverExt {
    /// The `robot.components` entry this driver drives (D47/D53), launched once
    /// per instance. Errors if the driver was launched without one.
    fn component(&self) -> crate::Result<&str>;
}

impl<R: Participant + IsDriver> SetupContextDriverExt for SetupContext<R> {
    fn component(&self) -> crate::Result<&str> {
        self.component_instance().ok_or_else(|| {
            anyhow::anyhow!("no component instance is bound (this driver was launched without one)")
        })
    }
}

/// Simulator-only `SetupContext` accessor
/// (`R: Participant + IsSimulator`); the simulator-marker twin of
/// [`SetupContextDriverExt`] (see its docs for why the two markers get
/// separate traits). A simulator that owns a per-component instance reads it
/// the same way a driver does.
pub trait SetupContextSimulatorExt {
    /// The bound `robot.components` instance, if the simulator was launched
    /// per instance. Errors otherwise.
    fn component(&self) -> crate::Result<&str>;
}

impl<R: Participant + IsSimulator> SetupContextSimulatorExt for SetupContext<R> {
    fn component(&self) -> crate::Result<&str> {
        self.component_instance().ok_or_else(|| {
            anyhow::anyhow!(
                "no component instance is bound (this simulator was launched without one)"
            )
        })
    }
}

/// Tool-only `SetupContext` accessor (`R: Participant + IsTool`). Tools stay
/// raw-bus only (decided 2026-07-09), so this is their sole IO seam.
pub trait SetupContextToolExt {
    /// Clone the runner-owned raw bus for privileged tool internals. The bus is
    /// already open from the launch contract, so a tool does not reparse launch
    /// env or open an unrelated session.
    fn raw_bus(&self) -> Bus;
}

impl<R: Participant + IsTool> SetupContextToolExt for SetupContext<R> {
    fn raw_bus(&self) -> Bus {
        self.bus().clone()
    }
}
