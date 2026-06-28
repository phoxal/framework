//! Proc-macros for the phoxal framework.
//!
//! Three macros make up the authoring surface:
//!
//! - [`phoxal_api_tree!`] — declares the dated API-version modules
//!   (`phoxal::api::y2026_1`, …), their version-local body types, the
//!   `ContractBody`/`ApiVersion` impls, and the api-local topic builders.
//! - [`derive@Runtime`] — reads a runtime struct's typed handle fields plus the
//!   `#[phoxal(id = …, api = …, config = …)]` attribute and emits the static
//!   metadata (`RuntimeFields`) the runner and `emit-apis` consume.
//! - [`macro@runtime`] — the bare `#[phoxal::runtime]` attribute on the inherent
//!   impl; it reads `#[setup]`/`#[step]`/`#[shutdown]` (and, in the query slice,
//!   `#[server]`/`#[server_snapshot]`/`#[snapshot]`) and emits the lifecycle
//!   dispatch (`RuntimeBehavior`).
//!
//! Generated code references the framework through `::phoxal::…`; the engine crate
//! makes that path resolve to itself with `extern crate self as phoxal;`.

mod api_tree;
mod runtime_derive;
mod runtime_impl;
mod util;

use proc_macro::TokenStream;

/// Declare a dated API-version tree of version-local wire bodies + topics.
///
/// See [`api_tree`] for the supported grammar. One invocation owns one or more
/// `version y2026_N { … }` blocks; each becomes a `pub mod y2026_N` under
/// wherever the macro is invoked (the engine invokes it inside `phoxal::api`).
#[proc_macro]
pub fn phoxal_api_tree(input: TokenStream) -> TokenStream {
    api_tree::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Derive the static metadata for a runtime struct.
///
/// Requires `#[phoxal(api = y2026_N)]` (mandatory) and accepts
/// `#[phoxal(id = "…")]` (optional; defaults to the kebab-cased type name) and
/// `#[phoxal(config = Type)]` (optional; user runtimes only).
#[proc_macro_derive(Runtime, attributes(phoxal))]
pub fn derive_runtime(input: TokenStream) -> TokenStream {
    runtime_derive::expand(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// The bare `#[phoxal::runtime]` attribute on a runtime's inherent impl.
///
/// Recognizes the lifecycle helper attributes and emits the runner-facing
/// dispatch while preserving the original methods.
#[proc_macro_attribute]
pub fn runtime(attr: TokenStream, item: TokenStream) -> TokenStream {
    runtime_impl::expand(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
