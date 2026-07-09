//! Shared helpers for the phoxal proc-macros.

use proc_macro2::TokenStream;
use quote::quote;

/// The path generated code uses to reach the framework. The engine crate makes
/// this resolve to itself via `extern crate self as phoxal;`.
pub fn phoxal() -> TokenStream {
    quote!(::phoxal)
}

/// The standard derive set applied to every macro-emitted wire body / helper
/// type: cloneable, comparable, debuggable, and serde round-trippable.
pub fn body_derives() -> TokenStream {
    quote!(#[derive(Clone, Debug, PartialEq, ::serde::Serialize, ::serde::Deserialize)])
}
