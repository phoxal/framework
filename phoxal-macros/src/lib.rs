//! Proc-macros for the phoxal framework.
//!
//! Three macro families make up the authoring surface:
//!
//! - [`phoxal_api_tree!`] - declares concrete API-revision modules
//!   (`phoxal_api::v0_1`, …), their revision-local body types, the
//!   `ContractBody`/`ApiVersion` impls, and the api-local topic builders.
//! - [`derive@Config`] - derives the config schema embedded in participant
//!   metadata.
//! - [`macro@service`] / [`macro@driver`] / [`macro@simulator`] /
//!   [`macro@brain`] - declare a unit marker's `Config`/`State`/`Api` types
//!   and identity (`ParticipantSpec`).
//! - [`macro@step`] - records cadence on the ordinary `Participant::step`
//!   override. Setup, reset, shutdown, and query handlers are plain Rust.
//!
//! The participant authoring macros (`macro@service` / `macro@driver` /
//! `macro@simulator` / `macro@brain` / `macro@step`) reference the framework
//! through `::phoxal::…`; the engine crate makes that path resolve to itself with
//! `extern crate self as phoxal;`. The
//! `phoxal_api_tree!` output instead targets the bus ABI floor directly as
//! `::phoxal_bus`, since it is invoked in the `phoxal-api` crate, which does not
//! depend on the engine.

mod api_tree;
mod authoring;

use proc_macro::TokenStream;

/// Declare a tree of wire bodies + typed topics, in one of two modes.
///
/// **API mode** (`version`) is the robot API: one invocation owns one or more
/// `version vM_N { … }` blocks and exactly one final `latest vM_N;`
/// declaration. A child may `extends` one earlier parent; inherited definitions
/// are fully materialized with the child's concrete identity. Additions are
/// direct and same-path changes require explicit `replace` or `remove`.
///
/// **Protocol mode** (`protocol`) is a process-boundary protocol such as the
/// supervisor's: one invocation owns one or more `protocol <name> { … }` trees,
/// with no revision axis at all - no `latest`, no `extends`/`replace`/`remove`,
/// and no version segment in the keys. See *Protocol mode* below. The two modes
/// are disjoint; one invocation is either one or the other.
///
/// The generated tree references the bus ABI floor as `::phoxal_bus`.
///
/// # Node grammar
///
/// A tree body is a tree of **nodes**. A node is either static (`name { … }`)
/// or dynamic (`name(var) { … }`, binding exactly one variable), and may nest to
/// any depth. Inside a node block, in any order:
///
/// - `struct …` / `enum …` - a version-local wire body. Macro-declared structs
///   get public fields; every body gets the standard derive set (`Clone`,
///   `Debug`, `PartialEq`, `serde::Serialize`/`Deserialize`).
/// - `topic <leaf>: command <Body>;` - a pub/sub topic the owning service
///   subscribes (a control input).
/// - `topic <leaf>: stream <Body>;` - a pub/sub topic the owning service
///   subscribes (ordered chunks with explicit saturation/close evidence).
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
///   version into the key makes differently-versioned contracts physically
///   distinct Zenoh keys.
/// - **body type path** - `phoxal_api::vM_N::<node>::…::<Body>`; variables never
///   appear in the module path.
///
/// A topic is dynamic when its node path contains at least one `(var)` node, and
/// static otherwise.
///
/// # Generated topic builders
///
/// Each version also gets an api-local `topic` module emitted with BOTH side
/// trees. `topic::client()` returns a `Root` for the PUBLIC **client** side;
/// `topic::owner()` returns a `Root` for the OWNER side. Both
/// have a method per node that walks the identical
/// tree (a dynamic node's method takes its variable as `impl Display`) and a leaf
/// method that returns a typed `bus::Topic<Kind>` with the key formatted from the
/// carried variables. The leaf brand is side-specific: on the client side a
/// `command` leaf is `Publish<Body>`, a `state` leaf is `Subscribe<Body>`, and a
/// `query` leaf is `AskQuery<Req, Resp>`; on the owner side those flip to
/// `Subscribe<Body>` / `Publish<Body>` / `ServeQuery<Req, Resp>`.
///
/// # Protocol mode
///
/// ```text
/// phoxal_api_tree! {
///     protocol supervisor {
///         connect {
///             #[serde(tag = "schema")]
///             enum Hello {
///                 #[serde(rename = "supervisor.hello/v0")]
///                 V0 { token: String },
///             }
///             topic hello: command Hello;   // key `supervisor/connect/hello`
///         }
///         run(execution) {
///             struct SnapshotRequest {}
///             struct Snapshot { running: bool }
///             topic snapshot: query SnapshotRequest => Snapshot;
///         }
///     }
/// }
/// ```
///
/// Node grammar, roles, query typing, dynamic segments, and both builder trees
/// are exactly as above. What differs:
///
/// - keys are **relative**: no `v0.1/` segment. The leading segment is the
///   protocol name, which is the same slot the dotted revision fills in API
///   mode, so a protocol topic composes under the bus's execution-scoped root
///   (`phoxal/<execution-id>/supervisor/connect/hello`) the same way a robot
///   API topic does;
/// - there is no revision history: `latest`, `extends`, `replace`, and `remove`
///   are all rejected. Pre-1.0, edit the declaration in place;
/// - the **developer owns the payload's schema version**. A document that
///   crosses a process boundary is authored as a serde-tagged enum whose
///   variants are its versions; this macro never reads a body's shape, never
///   infers a breaking change, and never mints a version;
/// - the generated marker's `ApiVersion::ID` (and each body's
///   `ContractBody::VERSION`) is the protocol name. The marker's job is
///   unchanged - it keeps one tree's bodies from being mistaken for another's
///   at compile time.
#[proc_macro]
pub fn phoxal_api_tree(input: TokenStream) -> TokenStream {
    api_tree::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declare the versioned robot API surface. This is the normal authoring
/// entry point; protocol trees use [`phoxal_protocol!`] instead.
#[proc_macro]
pub fn phoxal_api(input: TokenStream) -> TokenStream {
    api_tree::expand_api(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declare a process-boundary protocol surface without a revision/latest
/// axis. Keeping protocol mode in its own macro prevents the two grammars from
/// silently drifting into one another.
#[proc_macro]
pub fn phoxal_protocol(input: TokenStream) -> TokenStream {
    api_tree::expand_protocol(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Attach a positive, finite frequency to the ordinary
/// [`Participant::step`](https://docs.rs/phoxal) override.
#[proc_macro_attribute]
pub fn step(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring::expand_step(attr.into(), item.into())
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

/// Declare a service marker's `Config`/`State`/`Api` types. Each omitted type
/// defaults to `()`; identity defaults from `CARGO_PKG_NAME`.
///
/// An explicit `id` is still required whenever a crate defines more than one
/// participant - they cannot all default to the one package name - and
/// remains available any time the package name isn't the id you want.
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

/// Declare the one mandatory root brain: the robot project's composition root.
///
/// The brain is the root Cargo package's binary (`src/main.rs`). It is a
/// checked, clocked graph participant with exactly the typed-I/O and step
/// scheduling surface [`service`] has, and no privileged capability.
///
/// It differs from [`service`] in two fixed ways:
///
/// - its participant identity is always `brain`, so an `id = "…"` argument is
///   rejected - there is exactly one per robot project, and the CLI stages it
///   under the canonical `bin/brain`; and
/// - its `Config` is always `()`, so a `config = …` argument is rejected -
///   robot policy is ordinary Rust code compiled into this binary, not an
///   authored configuration side channel.
///
/// `state = …` and `api = …` work exactly as on every other checked role.
///
/// ```ignore
/// use phoxal::prelude::*;
///
/// #[phoxal::brain]
/// struct Brain;
///
/// impl Participant for Brain {
///     async fn setup(
///         &self,
///         _ctx: &mut SetupContext<Self>,
///         _config: Self::Config,
///     ) -> Result<(Self::State, Self::Api)> {
///         Ok(((), ()))
///     }
/// }
///
/// fn main() -> phoxal::Result<()> { phoxal::run::<Brain>() }
/// ```
#[proc_macro_attribute]
pub fn brain(attr: TokenStream, item: TokenStream) -> TokenStream {
    authoring::expand_participant(attr.into(), item.into(), authoring::ParticipantKind::Brain)
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
