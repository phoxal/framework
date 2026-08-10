//! Proc-macros for the phoxal framework.
//!
//! Three macro families make up the authoring surface:
//!
//! - [`phoxal_api_tree!`], [`phoxal_api_fragment!`], and
//!   [`phoxal_api_fragment_group!`] - author modular contract families.
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
//! `extern crate self as phoxal;`.

mod api_tree;
mod authoring;
mod wire_schema;

use proc_macro::TokenStream;

/// Declare the semantic endpoints for one authored contract module.
///
/// The fragment's `path` roots at its semantic family and is the wire key
/// prefix for every endpoint it declares. Payload types, implementations, and
/// tests remain ordinary sibling Rust items. Endpoint bodies name a local
/// payload type; the root tree's `source` path qualifies that name during
/// materialization.
#[proc_macro]
pub fn phoxal_api_fragment(input: TokenStream) -> TokenStream {
    api_tree::expand_fragment(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Relay child fragment registration without contributing a semantic path.
#[proc_macro]
pub fn phoxal_api_fragment_group(input: TokenStream) -> TokenStream {
    api_tree::expand_fragment_group(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Materialize semantic contract families from registered endpoint fragments.
///
/// Every fragment path roots at its family, and one generated module per family
/// collects the fragments that root there. The `source` path names the module
/// containing the family-first authored tree, such as `crate::api` for
/// `crate::api::robot::drive::Target`.
#[proc_macro]
pub fn phoxal_api_tree(input: TokenStream) -> TokenStream {
    api_tree::expand_tree(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Collector-chain target; public only because proc-macro exports must be.
#[doc(hidden)]
#[proc_macro]
pub fn __phoxal_api_materialize(input: TokenStream) -> TokenStream {
    api_tree::expand_materialized(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Nested group collector target; public only for macro expansion plumbing.
#[doc(hidden)]
#[proc_macro]
pub fn __phoxal_api_define_group(input: TokenStream) -> TokenStream {
    api_tree::expand_group_collector(input.into()).into()
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

/// Derive the serialized wire shape a type puts on the wire, read from the same
/// `#[serde(...)]` model `Serialize` itself reads.
///
/// The declared shape is what compatibility CI compares against a published
/// baseline, so a serde feature whose serialized form this cannot compute -
/// `flatten`, adjacent tagging, an arbitrary `serialize_with` - is a compile
/// error pointing at a hand-written
/// `phoxal_runtime_contract::wire_schema::DescribeWire`, never an approximate
/// schema.
#[proc_macro_derive(DescribeWire, attributes(serde))]
pub fn derive_describe_wire(input: TokenStream) -> TokenStream {
    wire_schema::expand(input.into())
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
