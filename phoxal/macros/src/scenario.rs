//! `#[phoxal::scenario]` proc-macro implementation.
//!
//! A scenario is an ordinary Rust test function whose only argument is a
//! mutable `phoxal::scenario::Simulation` fixture. The expansion adds
//! `#[test]`, constructs the fixture from the command-scoped test context,
//! and otherwise leaves Rust's test filtering, ignore, cfg, reporting, and
//! assertion behavior untouched.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{FnArg, ItemFn, Pat, ReturnType, Type};

pub fn expand_scenario(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            "#[phoxal::scenario] does not accept arguments",
        ));
    }

    let mut function = syn::parse2::<ItemFn>(item)?;
    validate_function(&function)?;

    let argument = function.sig.inputs.first().ok_or_else(|| {
        syn::Error::new_spanned(
            &function.sig,
            "#[phoxal::scenario] requires one `&mut Simulation` argument",
        )
    })?;
    let FnArg::Typed(argument) = argument else {
        return Err(syn::Error::new_spanned(
            argument,
            "#[phoxal::scenario] cannot be applied to a method",
        ));
    };
    let Pat::Ident(binding) = argument.pat.as_ref() else {
        return Err(syn::Error::new_spanned(
            &argument.pat,
            "the Simulation argument must use an identifier pattern",
        ));
    };
    let Type::Reference(reference) = argument.ty.as_ref() else {
        unreachable!("validate_function accepted a non-reference fixture")
    };
    let fixture_name = binding.ident.clone();
    let fixture_type = reference.elem.clone();
    let test_name = function.sig.ident.clone();
    let original = function.block.clone();

    function.sig.inputs.clear();
    function.attrs.push(syn::parse_quote!(#[test]));
    *function.block = syn::parse_quote!({
        let mut phoxal_simulation: #fixture_type =
            ::phoxal::scenario::Simulation::from_host_context(::std::concat!(
                ::std::module_path!(),
                "::",
                ::std::stringify!(#test_name),
            ))?;
        let #fixture_name = &mut phoxal_simulation;
        #original
    });

    Ok(quote!(#function))
}

fn validate_function(function: &ItemFn) -> syn::Result<()> {
    if function.sig.constness.is_some()
        || function.sig.asyncness.is_some()
        || function.sig.unsafety.is_some()
        || function.sig.abi.is_some()
        || !function.sig.generics.params.is_empty()
        || function.sig.variadic.is_some()
    {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "#[phoxal::scenario] requires a non-generic synchronous safe Rust function",
        ));
    }
    if function.sig.inputs.len() != 1 {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "#[phoxal::scenario] requires exactly one `&mut Simulation` argument",
        ));
    }
    let Some(FnArg::Typed(argument)) = function.sig.inputs.first() else {
        return Err(syn::Error::new_spanned(
            &function.sig.inputs,
            "#[phoxal::scenario] cannot be applied to a method",
        ));
    };
    let Type::Reference(reference) = argument.ty.as_ref() else {
        return Err(syn::Error::new_spanned(
            &argument.ty,
            "the scenario argument must be `&mut Simulation`",
        ));
    };
    if reference.mutability.is_none() {
        return Err(syn::Error::new_spanned(
            &argument.ty,
            "the scenario argument must be mutable: `&mut Simulation`",
        ));
    }
    let Type::Path(path) = reference.elem.as_ref() else {
        return Err(syn::Error::new_spanned(
            &reference.elem,
            "the scenario argument must be `&mut Simulation`",
        ));
    };
    if path
        .path
        .segments
        .last()
        .is_none_or(|part| part.ident != "Simulation")
    {
        return Err(syn::Error::new_spanned(
            &reference.elem,
            "the scenario argument type must be `Simulation`",
        ));
    }
    if matches!(function.sig.output, ReturnType::Default) {
        return Err(syn::Error::new_spanned(
            &function.sig,
            "a scenario function must return `phoxal::Result<()>`",
        ));
    }
    Ok(())
}
