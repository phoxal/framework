//! Proc-macros for the Phoxal Runtime.
//!
//! Runtime attributes collect typed bindings and derive the compile-time
//! configuration schema consumed by the Runtime artifact record.

mod api;
mod authoring;
mod inputs;
mod message;
mod outputs;
mod runtime;
mod scenario;

use proc_macro::TokenStream;

/// Declare one Rust-authored Protobuf message, enumeration, or payload
/// enum.
///
/// The attribute owns the Prost annotations and the retained schema record,
/// so the authored item is the single definition of its wire contract.
#[proc_macro_attribute]
pub fn message(attr: TokenStream, item: TokenStream) -> TokenStream {
    message::expand_message(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declare the Protobuf package of an inline contract module once for
/// every message, enumeration, and payload enum it contains.
#[proc_macro_attribute]
pub fn messages(attr: TokenStream, item: TokenStream) -> TokenStream {
    message::expand_messages(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Declare one Rust-authored endpoint contract.
///
/// The attributed struct is a compile-time declaration; the expansion owns
/// the generated local contract module. Named `endpoints` because the
/// crate-root `api!` macro keeps its established meaning: attaching the
/// composed robot's generated bindings.
#[proc_macro_attribute]
pub fn endpoints(attr: TokenStream, item: TokenStream) -> TokenStream {
    api::expand_api(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Register a synchronous `Runtime` implementation with authored cadence and
/// host-monotonic deadlines.
#[proc_macro_attribute]
pub fn runtime(attr: TokenStream, item: TokenStream) -> TokenStream {
    runtime::expand_runtime(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Turn a function taking `&mut phoxal::scenario::Simulation` into a standard
/// Rust test. See `phoxal::scenario` for the public surface.
#[proc_macro_attribute]
pub fn scenario(attr: TokenStream, item: TokenStream) -> TokenStream {
    scenario::expand_scenario(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Collect typed runtime input fields and their bounded policies.
#[proc_macro_attribute]
pub fn inputs(attr: TokenStream, item: TokenStream) -> TokenStream {
    inputs::expand_inputs(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Marker consumed by the `inputs` collector.
#[proc_macro_attribute]
pub fn input(attr: TokenStream, item: TokenStream) -> TokenStream {
    inputs::expand_input_marker(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

/// Collect transient fields or service output methods.
#[proc_macro_attribute]
pub fn outputs(attr: TokenStream, item: TokenStream) -> TokenStream {
    outputs::expand_outputs(attr.into(), item.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}

macro_rules! output_marker {
    ($name:ident, $role:ident) => {
        /// Marker consumed by the `outputs` collector.
        #[proc_macro_attribute]
        pub fn $name(attr: TokenStream, item: TokenStream) -> TokenStream {
            outputs::expand_marker(outputs::Role::$role, attr.into(), item.into())
                .unwrap_or_else(syn::Error::into_compile_error)
                .into()
        }
    };
}

output_marker!(state, State);
output_marker!(sample, Sample);
output_marker!(event, Event);
output_marker!(stream, Stream);
output_marker!(setpoint, Setpoint);
output_marker!(read, Read);
output_marker!(reply, Reply);
output_marker!(activate, Activate);
output_marker!(operation, Operation);

/// Derive a compile-time Draft 2020-12 JSON Schema from the same supported
/// `#[serde(...)]` attributes used by `Deserialize`.
#[proc_macro_derive(Config, attributes(serde))]
pub fn derive_config(input: TokenStream) -> TokenStream {
    authoring::expand_config(input.into())
        .unwrap_or_else(syn::Error::into_compile_error)
        .into()
}
