//! The one method-level participant macro: `#[phoxal::step(hz = N)]`.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::Parser;
use syn::{Expr, ExprLit, ImplItemFn, Lit, UnOp};

use crate::util::phoxal;

pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let hz = parse_hz(attr)?;
    let method: ImplItemFn = syn::parse2(item)?;

    if method.sig.ident != "step" {
        return Err(syn::Error::new_spanned(
            &method.sig.ident,
            "#[phoxal::step] must annotate the `step` Participant method",
        ));
    }
    if method.sig.asyncness.is_none() {
        return Err(syn::Error::new_spanned(
            &method.sig,
            "#[phoxal::step] requires an async method",
        ));
    }

    let phoxal = phoxal();
    Ok(quote! {
        #[doc(hidden)]
        fn __step_schedule() -> ::core::option::Option<#phoxal::__private::StepSchedule> {
            fn __assert_schedulable_surface<T: #phoxal::__private::SchedulableSurface>() {}
            __assert_schedulable_surface::<Self>();
            ::core::option::Option::Some(#phoxal::__private::StepSchedule::hz(#hz))
        }

        #method
    })
}

fn parse_hz(attr: TokenStream) -> syn::Result<f64> {
    let mut hz = None;
    let parser = syn::meta::parser(|meta| {
        if !meta.path.is_ident("hz") {
            return Err(meta.error("unknown #[phoxal::step(...)] key (expected `hz`)"));
        }
        if hz.is_some() {
            return Err(meta.error("duplicate `hz`"));
        }
        let value: Expr = meta.value()?.parse()?;
        hz = Some(expr_to_f64(&value)?);
        Ok(())
    });
    parser.parse2(attr)?;

    let hz = hz.ok_or_else(|| {
        syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[phoxal::step(hz = N)] requires a frequency",
        )
    })?;
    if !hz.is_finite() || hz <= 0.0 {
        return Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "#[phoxal::step(hz = N)] frequency must be positive and finite",
        ));
    }
    Ok(hz)
}

fn expr_to_f64(expr: &Expr) -> syn::Result<f64> {
    match expr {
        Expr::Lit(ExprLit {
            lit: Lit::Int(value),
            ..
        }) => value.base10_parse(),
        Expr::Lit(ExprLit {
            lit: Lit::Float(value),
            ..
        }) => value.base10_parse(),
        Expr::Unary(unary) if matches!(unary.op, UnOp::Neg(_)) => Ok(-expr_to_f64(&unary.expr)?),
        _ => Err(syn::Error::new_spanned(
            expr,
            "step frequency must be a numeric literal",
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use quote::quote;

    #[test]
    fn emits_the_hidden_schedule_hook_and_original_method() {
        let expanded = expand(
            quote!(hz = 50),
            quote! {
                async fn step(
                    &self,
                    _api: &Self::Api,
                    _step: StepContext,
                    _state: &mut Self::State,
                ) -> Result<()> {
                    Ok(())
                }
            },
        )
        .expect("step expands")
        .to_string();

        assert!(expanded.contains("__step_schedule"));
        assert!(expanded.contains("__assert_schedulable_surface"));
        assert!(expanded.contains("StepSchedule :: hz (50f64)"));
        assert!(expanded.contains("async fn step"));
    }
}
