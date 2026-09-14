//! `#[phoxal::scenario]` proc-macro implementation.
//!
//! Attribute on an `impl phoxal::scenario::Scenario for ConcreteType`
//! block. The macro:
//!
//! 1. Verifies the impl's trait ends in `Scenario`.
//! 2. Rejects generic impls and anonymous types.
//! 3. Generates one monomorphized `fn()` entry that constructs the
//!    scenario, calls `plan()`, and (in P3) `verify()`.
//! 4. Submits a [`ScenarioDescriptor`] into the static
//!    [`inventory::Inventory`] for the harness to discover.
//!
//! Per the plan the public identity is exactly `scenarios/<StructIdent>`.
//! The macro ignores the impl's file or module name.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{ItemImpl, Type};

pub fn expand_scenario(_attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let impl_block = syn::parse2::<ItemImpl>(item)?;

    // (1) Must have `impl ... for ...`.
    let (_, trait_path, _) = impl_block.trait_.as_ref().ok_or_else(|| {
        syn::Error::new_spanned(
            &impl_block,
            "#[phoxal::scenario] requires `impl <trait> for <type>`, not an inherent impl",
        )
    })?;

    // (2) The trait's last segment must be `Scenario`.
    let trait_ident = trait_path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new_spanned(trait_path, "empty trait path"))?;
    if trait_ident.ident != "Scenario" {
        let message = format!(
            "#[phoxal::scenario] requires an `impl ... Scenario for ...` block; found `{}`",
            trait_ident.ident
        );
        return Err(syn::Error::new_spanned(&trait_ident.ident, message));
    }

    // (3) Reject generic impls.
    if !impl_block.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &impl_block.generics,
            "#[phoxal::scenario] does not support generic impls; declare a concrete scenario struct",
        ));
    }

    // (4) Capture the concrete self type. Only `Type::Path` with a final
    //     ident is accepted; anonymous types, references, and generics are
    //     rejected with a clear diagnostic.
    let self_type = &impl_block.self_ty;
    let type_ident = match &**self_type {
        Type::Path(type_path) if type_path.qself.is_none() => type_path
            .path
            .segments
            .last()
            .ok_or_else(|| syn::Error::new_spanned(self_type, "expected a named scenario type"))?
            .ident
            .clone(),
        _ => {
            return Err(syn::Error::new_spanned(
                self_type,
                "#[phoxal::scenario] requires a concrete `pub struct` self type",
            ));
        }
    };

    // (5) Capture the starting line of the user's `impl` block so the
    //     registry can report a meaningful diagnostic location instead of
    //     a placeholder. `proc_macro2::Span::start()` resolves through
    //     both host toolchains (rustc and rust-analyzer) without falling
    //     back to `0`. `line` returns `usize`; we truncate to `u32` since
    //     line numbers greater than `u32::MAX` are not realistic for
    //     human-authored sources.
    let span_start_line: u32 =
        u32::try_from(impl_block.brace_token.span.open().start().line).unwrap_or(u32::MAX);

    let short_name = type_ident.to_string();
    let full_name = format!("scenarios/{short_name}");
    let entry_name = format_ident!("__phoxal_scenario_entry_{}", type_ident);

    // (6) Build the expansion. The user's impl is preserved verbatim; the
    //     entry function is monomorphized (one per impl block) and the
    //     descriptor registers it. `plan()` is called to validate the
    //     implementation at entry time; `verify()` lands in P3 once
    //     `ScenarioRun` carries evidence.
    let expanded = quote! {
        #impl_block

        #[doc(hidden)]
        #[allow(non_snake_case)]
        fn #entry_name() -> ::phoxal::Result<::phoxal::scenario::ScenarioOutcome> {
            // P1 ships the registration and dispatch path; execution lands
            // in P2-P3. Until then, attempting to run a scenario is an
            // explicit, unsupported error rather than a silent success.
            // The macro also fails compilation if the user's plan() does
            // not type-check, so the failure surface is consistent: an
            // authored impl that does not compile never registers.
            Err(::phoxal::anyhow!(
                "scenario `{}` is registered but its execution pipeline \
                 is not implemented in this build (P2-P3 pending)",
                #full_name,
            ))
        }

        ::phoxal::scenario::__macro::inventory::submit! {
            ::phoxal::scenario::ScenarioDescriptor {
                name: #full_name,
                short_name: #short_name,
                module_path: ::std::module_path!(),
                source_file: ::std::file!(),
                source_line: #span_start_line,
                entry: #entry_name,
            }
        }
    };

    Ok(expanded)
}

// Compile-time pin: forces this module to be a regular Rust module even
// when no other items reference its symbols, so editor tooling still sees
// the file.
#[allow(dead_code)]
fn _module_anchor(_span: Span) {}
