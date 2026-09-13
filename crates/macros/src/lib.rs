//! Proc-macros for the Phoxal Runtime.
//!
//! Runtime attributes collect typed bindings and derive the compile-time
//! configuration schema consumed by the Runtime artifact record.

mod authoring;
mod inputs;
mod outputs;
mod runtime;

use proc_macro::TokenStream;

/// Register a synchronous `Runtime` implementation with authored cadence and
/// host-monotonic deadlines.
#[proc_macro_attribute]
pub fn runtime(attr: TokenStream, item: TokenStream) -> TokenStream {
    runtime::expand_runtime(attr.into(), item.into())
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
