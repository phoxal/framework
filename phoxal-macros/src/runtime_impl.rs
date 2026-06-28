//! `#[phoxal::runtime]` — lifecycle dispatch from the runtime's inherent impl.
//!
//! Reads the lifecycle helper attributes on the methods of an inherent impl and
//! emits a `RuntimeBehavior` impl that the runner drives, while re-emitting the
//! original methods verbatim (helper attributes stripped).
//!
//! First slice: `#[setup]` (mandatory, exactly once), `#[step(hz = N)]` (≤ 1),
//! `#[shutdown]` (≤ 1). The query-serving attributes
//! (`#[server]`/`#[server_snapshot]`/`#[snapshot]`) are recognized and rejected
//! with a pointer to the query slice until that lands.

use proc_macro2::TokenStream;
use quote::quote;
use syn::spanned::Spanned;
use syn::{Attribute, Expr, ImplItem, ImplItemFn, ItemImpl, Lit, Meta};

pub fn expand(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new_spanned(
            attr,
            "#[phoxal::runtime] takes no arguments; configure the runtime on the struct via \
             #[derive(phoxal::Runtime)] #[phoxal(...)]",
        ));
    }

    let mut item_impl: ItemImpl = syn::parse2(item)?;
    let self_ty = (*item_impl.self_ty).clone();

    let mut setup: Option<LifecycleFn> = None;
    let mut step: Option<StepFn> = None;
    let mut shutdown: Option<LifecycleFn> = None;

    for impl_item in &mut item_impl.items {
        let ImplItem::Fn(method) = impl_item else {
            continue;
        };
        let Some(kind) = take_lifecycle_attr(method)? else {
            continue;
        };
        match kind {
            Lifecycle::Setup => {
                if setup.is_some() {
                    return Err(syn::Error::new(method.sig.span(), "duplicate #[setup]"));
                }
                if method.sig.ident != "setup" {
                    return Err(syn::Error::new(
                        method.sig.ident.span(),
                        "the #[setup] method must be named `setup` (D22)",
                    ));
                }
                setup = Some(LifecycleFn::parse_setup(method)?);
            }
            Lifecycle::Step(hz) => {
                if step.is_some() {
                    return Err(syn::Error::new(
                        method.sig.span(),
                        "duplicate #[step]: a runtime has at most one scheduled loop",
                    ));
                }
                step = Some(StepFn {
                    name: method.sig.ident.clone(),
                    hz,
                });
            }
            Lifecycle::Shutdown => {
                if shutdown.is_some() {
                    return Err(syn::Error::new(method.sig.span(), "duplicate #[shutdown]"));
                }
                shutdown = Some(LifecycleFn::parse_self_method(method)?);
            }
        }
    }

    let setup = setup.ok_or_else(|| {
        syn::Error::new_spanned(
            &item_impl.self_ty,
            "a runtime impl must declare exactly one #[setup] method (D22)",
        )
    })?;

    let setup_name = &setup.name;
    let setup_call = if setup.takes_extra_arg {
        quote!(Self::#setup_name(ctx, config).await)
    } else {
        quote!({
            let _ = config;
            Self::#setup_name(ctx).await
        })
    };

    let step_call = match &step {
        Some(s) => {
            let name = &s.name;
            quote!(self.#name(step).await)
        }
        None => quote!({
            let _ = step;
            ::core::result::Result::Ok(())
        }),
    };

    let step_schedule = match &step {
        Some(s) => {
            let hz = s.hz;
            quote!(::core::option::Option::Some(::phoxal::runtime::StepSchedule::hz(#hz)))
        }
        None => quote!(::core::option::Option::None),
    };

    let shutdown_call = match &shutdown {
        Some(s) => {
            let name = &s.name;
            if s.takes_extra_arg {
                quote!(self.#name(ctx).await)
            } else {
                quote!({
                    let _ = ctx;
                    self.#name().await
                })
            }
        }
        None => quote!({
            let _ = ctx;
            ::core::result::Result::Ok(())
        }),
    };

    Ok(quote! {
        #item_impl

        impl ::phoxal::runtime::RuntimeBehavior for #self_ty {
            const SERVER_CONTRACTS: &'static [::phoxal::runtime::ContractUse] = &[];

            fn __step_schedule() -> ::core::option::Option<::phoxal::runtime::StepSchedule> {
                #step_schedule
            }

            async fn __setup(
                ctx: &mut ::phoxal::runtime::SetupContext<Self>,
                config: <Self as ::phoxal::runtime::RuntimeFields>::Config,
            ) -> ::phoxal::Result<Self> {
                #setup_call
            }

            async fn __step(
                &mut self,
                step: ::phoxal::runtime::StepContext,
            ) -> ::phoxal::Result<()> {
                #step_call
            }

            async fn __shutdown(
                &mut self,
                ctx: ::phoxal::runtime::ShutdownContext,
            ) -> ::phoxal::Result<()> {
                #shutdown_call
            }
        }
    })
}

enum Lifecycle {
    Setup,
    Step(f64),
    Shutdown,
}

/// A lifecycle method that takes `&mut self`/`Self` plus an optional context arg.
struct LifecycleFn {
    name: syn::Ident,
    /// Whether the method declares an argument beyond the receiver/`ctx`.
    takes_extra_arg: bool,
}

impl LifecycleFn {
    /// `#[setup] async fn setup(ctx: &mut SetupContext<Self>[, config])`.
    fn parse_setup(method: &ImplItemFn) -> syn::Result<Self> {
        if method.sig.asyncness.is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[setup] must be `async`",
            ));
        }
        if method.sig.receiver().is_some() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[setup] is an associated function: it takes `ctx` (and optional `config`), not `self`",
            ));
        }
        let typed = method.sig.inputs.len();
        if typed == 0 {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[setup] must take `ctx: &mut SetupContext<Self>` as its first argument",
            ));
        }
        Ok(LifecycleFn {
            name: method.sig.ident.clone(),
            // first arg is ctx; a second arg means the runtime reads typed config.
            takes_extra_arg: typed >= 2,
        })
    }

    /// `#[shutdown] async fn shutdown(&mut self[, ctx: ShutdownContext])`.
    fn parse_self_method(method: &ImplItemFn) -> syn::Result<Self> {
        if method.sig.asyncness.is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[shutdown] must be `async`",
            ));
        }
        if method.sig.receiver().is_none() {
            return Err(syn::Error::new(
                method.sig.span(),
                "#[shutdown] takes `&mut self`",
            ));
        }
        // inputs includes the receiver; an extra typed arg is the ShutdownContext.
        let extra = method.sig.inputs.len() >= 2;
        Ok(LifecycleFn {
            name: method.sig.ident.clone(),
            takes_extra_arg: extra,
        })
    }
}

struct StepFn {
    name: syn::Ident,
    hz: f64,
}

/// Find and remove the single phoxal lifecycle attribute on a method, returning
/// which one it was. Errors on duplicates or query-slice attributes.
fn take_lifecycle_attr(method: &mut ImplItemFn) -> syn::Result<Option<Lifecycle>> {
    let mut found: Option<(usize, Lifecycle)> = None;

    for (idx, attr) in method.attrs.iter().enumerate() {
        let Some(name) = attr.path().get_ident().map(|i| i.to_string()) else {
            continue;
        };
        let kind = match name.as_str() {
            "setup" => {
                expect_path_only(attr, "setup")?;
                Lifecycle::Setup
            }
            "step" => Lifecycle::Step(parse_step_hz(attr)?),
            "shutdown" => {
                expect_path_only(attr, "shutdown")?;
                Lifecycle::Shutdown
            }
            "server" | "server_snapshot" | "snapshot" => {
                return Err(syn::Error::new_spanned(
                    attr,
                    format!(
                        "#[{name}] (query serving) lands in the query slice; the first slice \
                         supports #[setup], #[step], and #[shutdown] only"
                    ),
                ));
            }
            _ => continue,
        };
        if found.is_some() {
            return Err(syn::Error::new_spanned(
                attr,
                "a method may carry at most one phoxal lifecycle attribute",
            ));
        }
        found = Some((idx, kind));
    }

    if let Some((idx, kind)) = found {
        method.attrs.remove(idx);
        Ok(Some(kind))
    } else {
        Ok(None)
    }
}

fn expect_path_only(attr: &Attribute, name: &str) -> syn::Result<()> {
    match &attr.meta {
        Meta::Path(_) => Ok(()),
        _ => Err(syn::Error::new_spanned(
            attr,
            format!("#[{name}] takes no arguments"),
        )),
    }
}

/// Parse `#[step(hz = N)]` → frequency in Hz (positive, finite).
fn parse_step_hz(attr: &Attribute) -> syn::Result<f64> {
    let mut hz: Option<f64> = None;
    attr.parse_nested_meta(|meta| {
        if meta.path.is_ident("hz") {
            let value: Expr = meta.value()?.parse()?;
            hz = Some(expr_to_f64(&value)?);
            Ok(())
        } else {
            Err(meta.error("unknown #[step(...)] key (expected `hz`)"))
        }
    })?;
    let hz = hz.ok_or_else(|| {
        syn::Error::new_spanned(
            attr,
            "#[step(hz = N)] requires a frequency, e.g. #[step(hz = 50)]",
        )
    })?;
    if !(hz.is_finite() && hz > 0.0) {
        return Err(syn::Error::new_spanned(
            attr,
            "#[step(hz = N)] frequency must be positive and finite",
        ));
    }
    Ok(hz)
}

fn expr_to_f64(expr: &Expr) -> syn::Result<f64> {
    if let Expr::Lit(lit) = expr {
        match &lit.lit {
            Lit::Int(i) => return i.base10_parse::<f64>(),
            Lit::Float(f) => return f.base10_parse::<f64>(),
            _ => {}
        }
    }
    Err(syn::Error::new_spanned(
        expr,
        "expected a numeric literal for `hz`",
    ))
}
