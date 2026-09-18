//! Expansion for the root `runtime` registration attribute.

use heck::ToSnakeCase;
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::{Expr, ExprLit, ItemImpl, Lit, Type};

/// Expands `#[phoxal::runtime(period_ms = ..., timeout_ms = ..., init_timeout_ms = ...)]`.
pub fn expand_runtime(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let options = parse_options(attr)?;
    let implementation: ItemImpl = syn::parse2(item)?;
    let Some((_, trait_path, _)) = &implementation.trait_ else {
        return Err(syn::Error::new_spanned(
            &implementation,
            "#[phoxal::runtime] must be placed on `impl Runtime for Service`",
        ));
    };
    if trait_path
        .segments
        .last()
        .is_none_or(|segment| segment.ident != "Runtime")
    {
        return Err(syn::Error::new_spanned(
            trait_path,
            "#[phoxal::runtime] must be placed on `impl Runtime for Service`",
        ));
    }
    if !implementation.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &implementation.generics,
            "#[phoxal::runtime] does not support generic runtime implementations",
        ));
    }
    let self_type = (*implementation.self_ty).clone();
    let Type::Path(path) = &self_type else {
        return Err(syn::Error::new_spanned(
            &self_type,
            "#[phoxal::runtime] requires a named service type",
        ));
    };
    let service_name = path
        .path
        .segments
        .last()
        .map(|segment| segment.ident.clone())
        .ok_or_else(|| syn::Error::new_spanned(&self_type, "runtime service type has no name"))?;

    // Keep the trait implementation itself exactly as authored.  The nested
    // function checks make missing `#[inputs]` a compile error even when no
    // host runner is linked in the current crate.
    let check_name = format_ident!(
        "__phoxal_runtime_inputs_{}",
        service_name.to_string().to_snake_case()
    );
    let artifact_static = format_ident!(
        "__PHOXAL_RUNTIME_ARTIFACT_{}",
        service_name.to_string().to_snake_case()
    );
    let period = options.period_ms;
    let timeout = options.timeout_ms;
    let init_timeout = options.init_timeout_ms;
    Ok(quote! {
        #implementation

        impl ::phoxal::runtime::RegisteredRuntime for #self_type {
            const SPEC: ::phoxal::runtime::RuntimeSpec =
                ::phoxal::runtime::RuntimeSpec::from_millis(#period, #timeout, #init_timeout);

            #[doc(hidden)]
            fn __retain_artifact_metadata() {
                ::std::hint::black_box(&#artifact_static);
                for field in <Self::Inputs as ::phoxal::runtime::input::InputSet>::FIELDS {
                    if let Some(signature) = field.port_signature {
                        ::std::hint::black_box(signature.descriptor_set());
                    }
                }
                for field in <Self::Outputs as ::phoxal::runtime::outputs::OutputSet>::FIELDS {
                    if let Some(signature) = field.port_signature {
                        ::std::hint::black_box(signature.descriptor_set());
                    }
                }
                for field in <Self as ::phoxal::runtime::outputs::OutputBindings>::FIELDS {
                    if let Some(signature) = field.port_signature {
                        ::std::hint::black_box(signature.descriptor_set());
                    }
                }
            }
        }

        #[used]
        #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_art"))]
        #[cfg_attr(not(target_os = "macos"), unsafe(link_section = ".phoxal_art"))]
        #[doc(hidden)]
        static #artifact_static: ::phoxal::runtime::artifact::ArtifactRecord =
            ::phoxal::runtime::artifact::runtime_record(
                ::phoxal::runtime::RuntimeSpec::from_millis(#period, #timeout, #init_timeout),
                <<#self_type as ::phoxal::runtime::Runtime>::Config as ::phoxal::runtime::Config>::SCHEMA_JSON,
                <<#self_type as ::phoxal::runtime::Runtime>::Inputs as ::phoxal::runtime::input::InputSet>::FIELDS,
                <<#self_type as ::phoxal::runtime::Runtime>::Outputs as ::phoxal::runtime::outputs::OutputSet>::FIELDS,
                <#self_type as ::phoxal::runtime::outputs::OutputBindings>::FIELDS,
            );

        const fn #check_name<T: ::phoxal::runtime::input::InputSet>() {}
        const _: () = {
            #check_name::<<#self_type as ::phoxal::runtime::Runtime>::Inputs>();
            assert!(
                <#self_type as ::phoxal::runtime::RegisteredRuntime>::SPEC
                    .validate()
                    .is_ok(),
                "runtime timing values must be positive"
            );
        };
    })
}

#[derive(Clone, Copy)]
struct Options {
    period_ms: u64,
    timeout_ms: u64,
    init_timeout_ms: u64,
}

fn parse_options(attr: TokenStream) -> syn::Result<Options> {
    let mut period_ms = None;
    let mut timeout_ms = None;
    let mut init_timeout_ms = None;
    syn::meta::parser(|meta| {
        let name = meta
            .path
            .get_ident()
            .map(ToString::to_string)
            .ok_or_else(|| meta.error("runtime options must use identifier names"))?;
        let slot = match name.as_str() {
            "period_ms" => &mut period_ms,
            "timeout_ms" => &mut timeout_ms,
            "init_timeout_ms" => &mut init_timeout_ms,
            _ => return Err(meta.error(format!("unknown runtime option `{name}`"))),
        };
        if slot.is_some() {
            return Err(meta.error(format!("duplicate runtime option `{name}`")));
        }
        let expression: Expr = meta.value()?.parse()?;
        let Expr::Lit(ExprLit {
            lit: Lit::Int(value),
            ..
        }) = expression
        else {
            return Err(meta.error(format!("`{name}` must be a positive integer literal")));
        };
        let parsed = value.base10_parse::<u64>()?;
        if parsed == 0 {
            return Err(meta.error(format!("`{name}` must be positive")));
        }
        *slot = Some(parsed);
        Ok(())
    })
    .parse2(attr)?;
    Ok(Options {
        period_ms: period_ms
            .ok_or_else(|| syn::Error::new(Span::call_site(), "runtime requires period_ms"))?,
        timeout_ms: timeout_ms
            .ok_or_else(|| syn::Error::new(Span::call_site(), "runtime requires timeout_ms"))?,
        init_timeout_ms: init_timeout_ms.ok_or_else(|| {
            syn::Error::new(Span::call_site(), "runtime requires init_timeout_ms")
        })?,
    })
}
