//! Proc-macros for the phoxal framework.
//!
//! Three macro families make up the authoring surface:
//!
//! - [`phoxal_api_tree!`] - declares the dated API-version modules
//!   (`phoxal_api::y2026_1`, …), their version-local body types, the
//!   `ContractBody`/`ApiVersion` impls, and the api-local topic builders.
//! - [`derive@Service`] / [`derive@Driver`] / [`derive@Tool`] /
//!   [`derive@Simulator`] - read a participant struct's typed handle fields plus
//!   the `#[phoxal(id = …, api = …, config = …, contracts(…))]` attribute and
//!   emits the static metadata (`ParticipantSpec`) the runner and `emit-apis`
//!   consume.
//! - [`macro@behavior`] - the bare `#[phoxal::behavior]` attribute on the inherent
//!   impl; it reads `#[setup]`/`#[step(hz = N)]`/`#[shutdown]` plus the query-side
//!   `#[server]`/`#[server_snapshot]`/`#[snapshot]` and emits the lifecycle and
//!   server dispatch (`ParticipantBehavior`).
//!
//! The struct/impl macros are paired: the derive selects the API version, records
//! the artifact kind, and contributes the field-derived contracts, while
//! `#[phoxal::behavior]` adds the lifecycle methods and the server-side contracts.
//! Both reference the one API version chosen by the derive, and the generated
//! `ContractBody<Api = Self::Api>` assertions make a body from a different
//! version a compile error.
//!
//! The participant authoring macros (`derive@Service` / `derive@Driver` /
//! `derive@Tool` / `derive@Simulator` / `macro@behavior`) reference the framework
//! through `::phoxal::…`; the engine crate makes that path resolve to itself with
//! `extern crate self as phoxal;`. The
//! `phoxal_api_tree!` output instead targets the bus ABI floor directly as
//! `::phoxal_bus`, since it is invoked in the `phoxal-api` crate, which does not
//! depend on the engine.

mod api_tree;
mod authoring_v2;
mod behavior_v2;
mod runtime_derive;
mod runtime_impl;
mod setup_shape;
mod util;

use proc_macro::TokenStream;

/// Declare a dated API-version tree of version-local wire bodies + topics.
///
/// One invocation owns one or more `version y2026_N { … }` blocks; each becomes a
/// `pub mod y2026_N` under wherever the macro is invoked. A generation may be
/// authored as `preview version y2026_N`; it is still emitted at `y2026_N`, but
/// behind the per-generation Cargo feature `preview-y2026_N` and with
/// `ApiVersion::IS_PREVIEW = true`. In this workspace it is invoked in the
/// `phoxal-api` crate (the canonical import is
/// `use phoxal_api::y2026_1 as api;`), and the generated tree references the bus
/// ABI floor as `::phoxal_bus`. There is no `extends`: each generation is a
/// standalone, sparse batch (D1) - see the `api_tree` module docs.
///
/// # Node grammar
///
/// A version body is a tree of **nodes**. A node is either static (`name { … }`)
/// or dynamic (`name(var) { … }`, binding exactly one variable), and may nest to
/// any depth. Inside a node block, in any order:
///
/// - `struct …` / `enum …` - a version-local wire body. Macro-declared structs
///   get public fields; every body gets the standard derive set (`Clone`,
///   `Debug`, `PartialEq`, `serde::Serialize`/`Deserialize`).
/// - `topic <leaf>: command <Body>;` - a pub/sub topic the owning service
///   subscribes (a control input).
/// - `topic <leaf>: state <Body>;` - a pub/sub topic the owning service publishes
///   (telemetry/output). Same wire shape as `command`, but the side-branded
///   builders give it the inverse brand (see *Generated topic builders* below).
/// - `topic <leaf>: query <Req> => <Resp>;` - a request/response topic.
/// - a child node (`name { … }` / `name(var) { … }`).
///
/// Doc-comments and attributes attach to the next `struct`/`enum`; `topic`
/// declarations and child nodes take none.
///
/// # What each topic derives from its node path
///
/// A topic carries no per-topic params; its identity is derived from the path of
/// nodes enclosing it:
///
/// - **`TOPIC`** (the wire key) - the generation, then the `/`-joined node
///   segments plus the leaf, where a static node contributes `name` and a
///   dynamic node contributes `name/{var}` (e.g.
///   `y2026_N/component/{instance}/motor/{capability}/command`). Folding the
///   generation into the key (D1) is what makes two differently-versioned
///   contracts physically distinct Zenoh keys - there is no separate
///   `FAMILY`/`SCHEMA_ID` axis.
/// - **body type path** - `phoxal_api::y2026_N::<node>::…::<Body>`; variables never
///   appear in the module path.
///
/// A topic is dynamic when its node path contains at least one `(var)` node, and
/// static otherwise.
///
/// # Generated topic builders
///
/// Each version also gets an api-local `topic` module emitted with BOTH side trees
/// (L1, plan #00). `topic::new()` returns a `Root` for the PUBLIC **client** side;
/// `topic::internal::new(cap)` returns a `Root` for the OWNER side (a deliberate,
/// greppable owner opt-in). The owner entry requires the runner-minted
/// `phoxal_bus::OwnerCap` (L2): a participant passes `ctx.owner_capability()`. Both
/// have a method per node that walks the identical
/// tree (a dynamic node's method takes its variable as `impl Display`) and a leaf
/// method that returns a typed `bus::Topic<Kind>` with the key formatted from the
/// carried variables. The leaf brand is side-specific: on the client side a
/// `command` leaf is `Publish<Body>`, a `state` leaf is `Subscribe<Body>`, and a
/// `query` leaf is `AskQuery<Req, Resp>`; on the owner side those flip to
/// `Subscribe<Body>` / `Publish<Body>` / `ServeQuery<Req, Resp>`.
#[proc_macro]
pub fn phoxal_api_tree(input: TokenStream) -> TokenStream {
    api_tree::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive the static metadata for a service struct.
///
/// Applies to a non-generic struct with named fields. Emits an `impl
/// ParticipantSpec` (`KIND`, `PARTICIPANT_CLASS`, `ID`, `Api`/`API_VERSION`,
/// `Config`, and `FIELD_CONTRACTS`),
/// `DeclaresPublish`/`DeclaresSubscribe`/`DeclaresQuery`
/// marker impls, and a `ContractBody<Api = Self::Api>` assertion per body.
///
/// # `#[phoxal(...)]` attributes
///
/// - `api = y2026_N` - **mandatory**. Selects the one API version this participant
///   runs against; sets `type Api = phoxal_api::y2026_N::Api`. Compatibility is by
///   contract name: two participants interoperate on a contract iff they name the
///   exact same version-qualified body, which the generation-qualified wire key
///   makes physically impossible to get wrong (D1).
/// - `id = "…"` - optional. The participant id; defaults to the kebab-cased type name.
/// - `config = Type` - optional. The participant's config type; defaults to `()`.
/// - `contracts(…)` - optional. Declares IO the derive cannot see in fields, as a
///   comma-separated list of `publishes(Body)`, `subscribes(Body)`, and
///   `queries(Req => Resp)` (`->` is also accepted) directives.
///
/// # Field-derived contracts
///
/// Each field is matched by **canonical syntactic form** of its type; everything
/// else is ignored as participant-private state. `Publisher<T>` is a publish,
/// `Subscriber<T>` and `Latest<T>` are subscribes, and `Querier<A, B>` is a query.
/// A `Vec<Handle>` carries the element handle, and a `BTreeMap<K, Handle>` /
/// `HashMap<K, Handle>` carries the value handle. Field-derived and `contracts(…)`
/// declarations are merged and deduplicated into `FIELD_CONTRACTS`.
#[proc_macro_derive(Service, attributes(phoxal))]
pub fn derive_service(input: TokenStream) -> TokenStream {
    runtime_derive::expand(input.into(), runtime_derive::AuthoringKind::Service)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive the static metadata for a component driver struct.
///
/// This is the driver-shaped counterpart to [`derive@Service`]. It records
/// `KIND = "driver"` and emits the marker that makes `SetupContext::component()`
/// available to the participant type.
#[proc_macro_derive(Driver, attributes(phoxal))]
pub fn derive_driver(input: TokenStream) -> TokenStream {
    runtime_derive::expand(input.into(), runtime_derive::AuthoringKind::Driver)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive the static metadata for a host-side tool struct.
///
/// Tools record `KIND = "tool"`, emit the `IsTool` marker, and are reported as
/// `participant_class = "privileged"`. Privileged raw-bus access is intentionally
/// a separate framework opt-in at `phoxal::raw`, not part of the default checked
/// participant surface.
#[proc_macro_derive(Tool, attributes(phoxal))]
pub fn derive_tool(input: TokenStream) -> TokenStream {
    runtime_derive::expand(input.into(), runtime_derive::AuthoringKind::Tool)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive the static metadata for a simulation participant struct.
///
/// Simulators record `KIND = "simulator"` and emit a marker for later
/// simulation-clock ownership.
#[proc_macro_derive(Simulator, attributes(phoxal))]
pub fn derive_simulator(input: TokenStream) -> TokenStream {
    runtime_derive::expand(input.into(), runtime_derive::AuthoringKind::Simulator)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// The bare `#[phoxal::behavior]` attribute on a participant's inherent impl.
///
/// Takes no arguments (configure the participant on the struct via
/// `#[derive(Service)] #[phoxal(...)]`, `#[derive(Driver)] #[phoxal(...)]`,
/// `#[derive(Tool)] #[phoxal(...)]`, or `#[derive(Simulator)] #[phoxal(...)]`).
/// Reads the lifecycle/server helper
/// attributes on the impl's methods, emits a `ParticipantBehavior` impl that the
/// runner drives, and re-emits the original methods verbatim with the helper
/// attributes stripped. A method may carry at most one helper attribute.
///
/// # Lifecycle / server attributes and their required signatures
///
/// - `#[setup]` - **mandatory, exactly once**. An `async` associated function
///   named `setup` taking `ctx: &mut SetupContext<Self>` and, optionally, the
///   participant config; returns `Result<Self>`.
/// - `#[step(hz = N)]` - at most once. `async fn (&mut self, step: StepContext)
///   -> Result<()>`; the scheduled control loop runs at the positive, finite
///   frequency `N`.
/// - `#[shutdown]` - at most once. An `async` method named `shutdown` taking
///   `&mut self` and, optionally, `ctx: ShutdownContext`; returns `Result<()>`.
/// - `#[server(topic = …)]` - an exclusive query server: `async fn (&mut self,
///   request: Req) -> ServerResult<Resp>`. Serialized with `#[step]`.
/// - `#[server_snapshot(topic = …)]` - a concurrent, read-only query server: an
///   `async` associated function taking `state: Snapshot<State>` and `request:
///   Req`, returning `ServerResult<Resp>`. Requires a `#[snapshot]` provider.
/// - `#[snapshot]` - at most once. The committed-snapshot provider: a synchronous
///   `fn (&self) -> State` returning the committed state.
///
/// For `#[server]`/`#[server_snapshot]` the `topic = …` is an api-local topic
/// builder; its key must match the request body's `TOPIC`, and both request and
/// response bodies must be `ContractBody<Api = Self::Api>` (checked at compile
/// time; a query only ever reaches the handler on its own generation-qualified
/// topic key, D1, so there is no separate decode-time identity check left).
///
/// # `Tool` is a thin runner
///
/// A `#[derive(Tool)]` participant may use `#[setup]` and `#[shutdown]` (plus
/// `#[snapshot]`, which is inert without a server), but `#[step]`,
/// `#[server(...)]`, and `#[server_snapshot(...)]` are the typed-graph surface and
/// are a compile error on a `Tool`: tools are privileged, out-of-band, thin
/// raw-bus runners (lifecycle + `sd_notify` + `participant_id` + `ctx.robot()` +
/// `phoxal::raw`), not checked participants. A tool that needs a recurring loop
/// spawns and owns its own task from `#[setup]`.
#[proc_macro_attribute]
pub fn behavior(attr: TokenStream, item: TokenStream) -> TokenStream {
    runtime_impl::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive the bus-facing contract surface from an `Api` handle struct (NEW
/// authoring model). See `phoxal::participant::api` for the trait shape and
/// how this coexists with the derives above.
///
/// Scans fields by canonical syntactic form exactly like `derive@Service`
/// (`Publisher<T>` / `Subscriber<T>` / `Latest<T>` / `Vec`/map thereof), plus
/// the new `Server<Req, Resp>` field kind (a served query contract - no live
/// connection, declared for `#[phoxal::behavior]`'s `#[server(api = …)]` /
/// `#[server_snapshot(api = …)]` to implement). Unlike the old model, there is
/// no participant-level API version: fields may name contracts from different
/// generations. Also emits a `#[used]` linker-section static recording each
/// field's role and version-qualified contract type (compile-time embedded
/// metadata, cargo-auditable pattern) - see the module docs for what this
/// slice does and does not materialize.
#[proc_macro_derive(Api)]
pub fn derive_api(input: TokenStream) -> TokenStream {
    authoring_v2::expand_api(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive participant config identity from a `Config` struct (NEW authoring
/// model). `SCHEMA_JSON` is a placeholder in this slice; see
/// `phoxal::participant::api::ParticipantConfig`.
#[proc_macro_derive(Config)]
pub fn derive_config(input: TokenStream) -> TokenStream {
    authoring_v2::expand_config(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Link a participant state struct to its `Config`/`Api` types as a checked
/// service (NEW authoring model - distinct from `#[derive(phoxal::Service)]`,
/// which scans the struct itself for handle fields). Defaults to the local
/// `Config`/`Api` type names; override with `#[phoxal::service(id = "…",
/// config = Type, api = Type)]`.
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring_v2::expand_participant(
        attr.into(),
        item.into(),
        authoring_v2::ParticipantKind::Service,
    )
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

/// The driver-shaped counterpart to [`service`] (NEW authoring model).
#[proc_macro_attribute]
pub fn driver(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring_v2::expand_participant(
        attr.into(),
        item.into(),
        authoring_v2::ParticipantKind::Driver,
    )
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

/// The simulator-shaped counterpart to [`service`] (NEW authoring model).
#[proc_macro_attribute]
pub fn simulator(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring_v2::expand_participant(
        attr.into(),
        item.into(),
        authoring_v2::ParticipantKind::Simulator,
    )
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

/// The tool-shaped counterpart to [`service`] (NEW authoring model). `Api`
/// defaults to `()` - tools stay raw-bus only (decided 2026-07-09).
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring_v2::expand_participant(
        attr.into(),
        item.into(),
        authoring_v2::ParticipantKind::Tool,
    )
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}
