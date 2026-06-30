//! Shared helpers for the phoxal proc-macros.

use proc_macro2::TokenStream;
use quote::quote;

/// The path generated code uses to reach the framework. The engine crate makes
/// this resolve to itself via `extern crate self as phoxal;`.
pub fn phoxal() -> TokenStream {
    quote!(::phoxal)
}

/// The path generated code uses to reach the dated API contract tree (the
/// `y2026_N` version modules + their `Api` marker type). These live in the
/// `phoxal-api` crate, which every runtime depends on directly (and the engine
/// pulls in as a dev-dependency for its own tests/examples). The `#[derive(Runtime)]`
/// output names `phoxal_api::y2026_N::Api` to bind a runtime's one API version,
/// since that marker type lives only here and is not re-exported through the
/// engine's `phoxal::bus` ABI floor.
pub fn phoxal_api() -> TokenStream {
    quote!(::phoxal_api)
}

/// The standard derive set applied to every macro-emitted wire body / helper
/// type: cloneable, comparable, debuggable, and serde round-trippable.
pub fn body_derives() -> TokenStream {
    quote!(#[derive(Clone, Debug, PartialEq, ::serde::Serialize, ::serde::Deserialize)])
}
