//! Proc-macros for the phoxal framework.
//!
//! Three macro families make up the authoring surface:
//!
//! - [`phoxal_api_tree!`] - declares concrete API-revision modules
//!   (`phoxal_api::v0_1`, …), their revision-local body types, the
//!   `ContractBody`/`ApiVersion` impls, and the api-local topic builders.
//! - [`derive@Api`] / [`derive@Config`] - read an `Api` handle struct's typed
//!   fields (`Publisher<T>` / `Subscriber<T>` / `Latest<T>` / `Querier<Req,
//!   Resp>` / `Server<Req, Resp>`) and a `Config` struct respectively, and emit
//!   the static metadata (`ParticipantApi`/`ParticipantConfig`) the runner
//!   consumes.
//! - [`macro@service`] / [`macro@driver`] / [`macro@simulator`] / [`macro@tool`] -
//!   link a participant state struct to its `Config`/`Api` types and record
//!   its identity (`Participant`).
//! - [`macro@behavior`] - the bare `#[phoxal::behavior]` attribute on the inherent
//!   impl; it reads `#[setup]`/`#[step(hz = N)]`/`#[shutdown]` plus the query-side
//!   `#[server]`/`#[server_snapshot]`/`#[snapshot]` and emits the lifecycle and
//!   server dispatch (`ParticipantLifecycle`).
//!
//! The struct/impl macros are paired: `#[phoxal::service|driver|simulator|tool]`
//! links the participant to its `Config`/`Api` types and records the artifact
//! kind, while `#[phoxal::behavior]` adds the lifecycle methods and the
//! server-side contracts, threading `Self::Api` through every callback (D3).
//!
//! The participant authoring macros (`macro@service` / `macro@driver` /
//! `macro@tool` / `macro@simulator` / `macro@behavior`) reference the framework
//! through `::phoxal::…`; the engine crate makes that path resolve to itself with
//! `extern crate self as phoxal;`. The
//! `phoxal_api_tree!` output instead targets the bus ABI floor directly as
//! `::phoxal_bus`, since it is invoked in the `phoxal-api` crate, which does not
//! depend on the engine.

mod api_tree;
mod authoring;
mod behavior;
mod util;

use proc_macro::TokenStream;

/// Declare a versioned API tree of version-local wire bodies + topics.
///
/// One invocation owns one or more `version vM_N { … }` blocks and exactly one
/// final `latest vM_N;` declaration. A child may `extends` one earlier parent;
/// inherited definitions are fully materialized with the child's concrete
/// identity. Additions are direct and same-path changes require explicit
/// `replace` or `remove`. The generated tree references the bus ABI floor as
/// `::phoxal_bus`.
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
/// - **`TOPIC`** (the wire key) - the version, then the `/`-joined node
///   segments plus the leaf, where a static node contributes `name` and a
///   dynamic node contributes `name/{var}` (e.g.
///   `v0.1/component/{instance}/motor/{capability}/command`). Folding the
///   version into the key (D1) is what makes two differently-versioned
///   contracts physically distinct Zenoh keys - there is no separate
///   `FAMILY`/`SCHEMA_ID` axis.
/// - **body type path** - `phoxal_api::vM_N::<node>::…::<Body>`; variables never
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

/// The bare `#[phoxal::behavior]` attribute on a participant's inherent impl.
///
/// Takes no arguments (configure the participant on the struct via
/// `#[phoxal::service]`, `#[phoxal::driver]`, `#[phoxal::tool]`, or
/// `#[phoxal::simulator]`, and its bus-facing handles via `#[derive(phoxal::Api)]`
/// on a companion `Api` struct). Reads the lifecycle/server helper attributes on
/// the impl's methods, emits a `ParticipantLifecycle` impl that the runner
/// drives, and re-emits the original methods verbatim with the helper
/// attributes stripped. A method may carry at most one helper attribute.
///
/// # Lifecycle / server attributes and their required signatures
///
/// - `#[setup]` - **mandatory, exactly once**. An `async` associated function
///   named `setup` taking `ctx: &mut SetupContext<Self>` and, optionally, the
///   participant config; returns `Result<(Self, Self::Api)>`.
/// - `#[step(hz = N)]` - at most once. `async fn (&mut self, api: &mut Self::Api,
///   step: StepContext) -> Result<()>`; the scheduled control loop runs at the
///   positive, finite frequency `N`.
/// - `#[shutdown]` - at most once. An `async` method named `shutdown` taking
///   `&mut self`, `api: &mut Self::Api`, and, optionally, `ctx: ShutdownContext`;
///   returns `Result<()>`.
/// - `#[server(api = field)]` - an exclusive query server: `async fn (&mut self,
///   api: &mut Self::Api, request: Req) -> ServerResult<Resp>`. Serialized with
///   `#[step]`.
/// - `#[server_snapshot(api = field)]` - a concurrent, read-only query server: an
///   `async` associated function taking `state: Snapshot<State>`, `api:
///   &Self::Api`, and `request: Req`, returning `ServerResult<Resp>`. Requires a
///   `#[snapshot]` provider.
/// - `#[snapshot]` - at most once. The committed-snapshot provider: a synchronous
///   `fn (&self) -> State` returning the committed state.
///
/// For `#[server]`/`#[server_snapshot]` the `api = field` names the `Api` struct's
/// `Server<Req, Resp>` field being implemented; both request and response bodies
/// must be `ContractBody` (checked at compile time; a query only ever reaches the
/// handler on its own version-qualified topic key, D1, so there is no
/// separate decode-time identity check left).
///
/// # A `tool` is a thin runner
///
/// A `#[phoxal::tool]` participant may use `#[setup]` and `#[shutdown]` (plus
/// `#[snapshot]`, which is inert without a server), but `#[step]`,
/// `#[server(...)]`, and `#[server_snapshot(...)]` are the typed-graph surface and
/// are a compile error on a tool: tools are privileged, out-of-band, thin
/// raw-bus runners (lifecycle + `participant_id` + `ctx.robot()` +
/// `phoxal::raw`), not checked participants. A tool that needs a recurring loop
/// spawns and owns its own task from `#[setup]`.
#[proc_macro_attribute]
pub fn behavior(attr: TokenStream, item: TokenStream) -> TokenStream {
    behavior::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive the bus-facing contract surface from an `Api` handle struct. See
/// `phoxal::participant::api` for the trait shape.
///
/// Scans fields by canonical syntactic form: `Publisher<T>` / `Subscriber<T>` /
/// `Latest<T>` are pub/sub handles, `Querier<Req, Resp>` is the asking side of a
/// query, and `Server<Req, Resp>` is a served query contract - no live
/// connection, declared for `#[phoxal::behavior]`'s `#[server(api = …)]` /
/// `#[server_snapshot(api = …)]` to implement. A `Vec`/`BTreeMap`/`HashMap` of a
/// handle carries the inner handle's declaration. Official participants name
/// the train-selected complete revision through `phoxal::api`. Also
/// emits the const contract JSON fragment that the participant attribute puts
/// in the linker section alongside its config schema.
///
/// A `subscribe`/`ask` field may carry `#[phoxal(external)]` (coherence-gate
/// design doc §1) to mark that its counterpart legitimately lives outside the
/// checked participant set; it is a compile error on a `publish`/`serve`
/// field, and a compile error for two fields naming the same `(role,
/// contract)` to disagree on the marker.
#[proc_macro_derive(Api, attributes(phoxal))]
pub fn derive_api(input: TokenStream) -> TokenStream {
    authoring::expand_api(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive a compile-time Draft 2020-12 JSON Schema from the same supported
/// `#[serde(...)]` attributes used by `Deserialize`: `rename`, `rename_all`,
/// `default`, and `deny_unknown_fields`. Unsupported Serde attributes are a
/// compile error rather than an approximate schema.
#[proc_macro_derive(Config, attributes(serde))]
pub fn derive_config(input: TokenStream) -> TokenStream {
    authoring::expand_config(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Link a participant state struct to its `Config`/`Api` types as a checked
/// service. Defaults to the local `Config`/`Api` type names; override with
/// `#[phoxal::service(id = "…", config = Type, api = Type)]`.
///
/// For user runtimes, `Config` is the user-authored `robot.yaml` surface. A
/// framework runtime may use this same slot for a CLI-synthesized launch
/// payload (for example a cross-robot staging product); ordinary framework
/// knobs belong in the robot model received through `ctx.robot()`.
#[proc_macro_attribute]
pub fn service(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring::expand_participant(
        attr.into(),
        item.into(),
        authoring::ParticipantKind::Service,
    )
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

/// The driver-shaped counterpart to [`service`].
#[proc_macro_attribute]
pub fn driver(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring::expand_participant(attr.into(), item.into(), authoring::ParticipantKind::Driver)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// The simulator-shaped counterpart to [`service`].
#[proc_macro_attribute]
pub fn simulator(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring::expand_participant(
        attr.into(),
        item.into(),
        authoring::ParticipantKind::Simulator,
    )
    .unwrap_or_else(syn::Error::into_compile_error)
    .into()
}

/// The tool-shaped counterpart to [`service`]. `Api` and `Config` default to
/// `()` - tools stay raw-bus only (decided 2026-07-09), and a configless tool
/// can launch without `PHOXAL_CONFIG`. An explicit `config = Type` remains
/// required at launch unless that type itself accepts `null` (for example,
/// `Option<T>`).
#[proc_macro_attribute]
pub fn tool(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring::expand_participant(attr.into(), item.into(), authoring::ParticipantKind::Tool)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
