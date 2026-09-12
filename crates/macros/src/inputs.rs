//! Expansion for the `runtime::inputs` collector.

use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::{Expr, ExprLit, Field, Fields, GenericArgument, Ident, ItemStruct, Lit, Type};

#[derive(Default)]
struct Options {
    max_age_ms: Option<u64>,
    max_items: Option<u64>,
    max_bytes: Option<u64>,
    port: Option<Expr>,
}

/// Expands `#[phoxal::runtime::inputs]` and consumes nested input markers.
pub fn expand_inputs(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    if !attr.is_empty() {
        return Err(syn::Error::new(
            Span::call_site(),
            "#[phoxal::runtime::inputs] takes no arguments",
        ));
    }
    let mut input: ItemStruct = syn::parse2(item)?;
    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[phoxal::runtime::inputs] does not support generic input structs",
        ));
    }
    let name = input.ident.clone();
    let fields = match &mut input.fields {
        Fields::Named(fields) => &mut fields.named,
        _ => {
            return Err(syn::Error::new_spanned(
                &input,
                "#[phoxal::runtime::inputs] requires a struct with named fields",
            ));
        }
    };

    let mut metadata = Vec::new();
    let mut checks = Vec::new();
    let mut bindings = Vec::new();
    let mut field_names = Vec::new();
    for field in fields.iter_mut() {
        let Some(field_name) = field.ident.clone() else {
            return Err(syn::Error::new_spanned(
                field,
                "runtime input fields need names",
            ));
        };
        let ty = field.ty.clone();
        field_names.push(field_name.clone());
        let kind = input_kind(&ty)?;
        let mut options = Options::default();
        let mut retained = Vec::new();
        for attribute in field.attrs.drain(..) {
            if attribute
                .path()
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "input")
            {
                parse_options(&attribute, kind, &mut options)?;
            } else {
                retained.push(attribute);
            }
        }
        field.attrs = retained;
        validate_options(kind, &options, field)?;
        let kind_variant = Ident::new(kind.variant(), field_name.span());
        let max_age = option_tokens(options.max_age_ms);
        let max_items = option_tokens(options.max_items);
        let max_bytes = option_tokens(options.max_bytes);
        let port = options
            .port
            .as_ref()
            .map_or_else(|| quote!(None), |port| quote!(Some((#port).name())));
        let port_signature = options
            .port
            .as_ref()
            .map_or_else(|| quote!(None), |port| quote!(Some((#port).signature())));
        metadata.push(quote! {
            ::phoxal::runtime::input::InputField {
                name: stringify!(#field_name),
                kind: ::phoxal::runtime::input::InputKind::#kind_variant,
                max_age_ms: #max_age,
                max_items: #max_items,
                max_bytes: #max_bytes,
                port: #port,
                port_signature: #port_signature,
            }
        });
        checks.push(input_type_check(&ty, &field_name));
        let field_id = field_id(&field_name);
        bindings.push(quote! {
            impl ::phoxal::runtime::input::InputFieldBinding<{#field_id}> for #name {
                type Form = #ty;
            }
        });
        if kind == InputKind::Commands {
            let (request, response) = generic_types(&ty, 2)?;
            let check_name = format_ident!("__phoxal_commands_port_{}", field_name);
            let port = options
                .port
                .as_ref()
                .ok_or_else(|| syn::Error::new_spanned(field, "Commands requires port = ..."))?;
            checks.push(quote! {
                fn #check_name() {
                    ::phoxal::runtime::__private::assert_commands_port::<_, #request, #response>(#port);
                }
            });
        }
    }

    Ok(quote! {
        #input

        impl ::phoxal::runtime::input::InputSet for #name {
            const FIELDS: &'static [::phoxal::runtime::input::InputField] = &[#(#metadata),*];
        }

        impl ::phoxal::runtime::input::InputSnapshot for #name {
            fn empty() -> Self {
                Self {
                    #(#field_names: ::core::default::Default::default(),)*
                }
            }
        }

        #(#bindings)*

        const _: () = {
            #(#checks)*
        };
    })
}

/// A direct nested marker is consumed by its enclosing collector.  Keeping a
/// real proc macro for it makes malformed placement produce a focused error.
pub fn expand_input_marker(_attr: TokenStream, _item: TokenStream) -> syn::Result<TokenStream> {
    Err(syn::Error::new(
        Span::call_site(),
        "#[phoxal::runtime::input] is valid only on a field inside #[phoxal::runtime::inputs]",
    ))
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum InputKind {
    Latest,
    Samples,
    Events,
    Setpoint,
    Stream,
    Commands,
    Read,
    Request,
    Operation,
}

impl InputKind {
    fn variant(self) -> &'static str {
        match self {
            Self::Latest => "Latest",
            Self::Samples => "Samples",
            Self::Events => "Events",
            Self::Setpoint => "Setpoint",
            Self::Stream => "Stream",
            Self::Commands => "Commands",
            Self::Read => "Read",
            Self::Request => "Request",
            Self::Operation => "Operation",
        }
    }
}

fn input_kind(ty: &Type) -> syn::Result<InputKind> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "runtime inputs must use one of input::Latest, Samples, Events, Setpoint, Stream, Commands, Read, Request, or Operation",
        ));
    };
    let name = path
        .path
        .segments
        .last()
        .map(|segment| segment.ident.to_string())
        .ok_or_else(|| syn::Error::new_spanned(ty, "runtime input type has no name"))?;
    match name.as_str() {
        "Latest" => Ok(InputKind::Latest),
        "Samples" => Ok(InputKind::Samples),
        "Events" => Ok(InputKind::Events),
        "Setpoint" => Ok(InputKind::Setpoint),
        "Stream" => Ok(InputKind::Stream),
        "Commands" => Ok(InputKind::Commands),
        "Read" => Ok(InputKind::Read),
        "Request" => Ok(InputKind::Request),
        "Operation" => Ok(InputKind::Operation),
        _ => Err(syn::Error::new_spanned(
            ty,
            "unsupported runtime input type; use a type from phoxal::runtime::input",
        )),
    }
}

fn parse_options(
    attribute: &syn::Attribute,
    kind: InputKind,
    options: &mut Options,
) -> syn::Result<()> {
    attribute.parse_nested_meta(|meta| {
        let name = meta
            .path
            .get_ident()
            .map(Ident::to_string)
            .ok_or_else(|| meta.error("runtime input options must use identifier names"))?;
        match name.as_str() {
            "max_age_ms" if kind == InputKind::Latest => {
                set_u64(&mut options.max_age_ms, &meta, "max_age_ms")?;
            }
            "max_items"
                if matches!(
                    kind,
                    InputKind::Samples
                        | InputKind::Events
                        | InputKind::Stream
                        | InputKind::Commands
                ) =>
            {
                set_u64(&mut options.max_items, &meta, "max_items")?;
            }
            "max_bytes"
                if matches!(
                    kind,
                    InputKind::Samples
                        | InputKind::Events
                        | InputKind::Stream
                        | InputKind::Commands
                ) =>
            {
                set_u64(&mut options.max_bytes, &meta, "max_bytes")?;
            }
            "max_response_bytes" if matches!(kind, InputKind::Read | InputKind::Request) => {
                set_u64(&mut options.max_bytes, &meta, "max_response_bytes")?;
            }
            "port" if kind == InputKind::Commands => {
                if options.port.is_some() {
                    return Err(meta.error("duplicate runtime input option `port`"));
                }
                options.port = Some(meta.value()?.parse::<Expr>()?);
            }
            _ => {
                return Err(meta.error(format!(
                    "option `{name}` is not allowed on this runtime input type"
                )));
            }
        }
        Ok(())
    })
}

fn set_u64(
    slot: &mut Option<u64>,
    meta: &syn::meta::ParseNestedMeta<'_>,
    name: &str,
) -> syn::Result<()> {
    if slot.is_some() {
        return Err(meta.error(format!("duplicate runtime input option `{name}`")));
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
}

fn validate_options(kind: InputKind, options: &Options, field: &Field) -> syn::Result<()> {
    let needs_batch_bound = matches!(
        kind,
        InputKind::Samples | InputKind::Events | InputKind::Stream | InputKind::Commands
    );
    if needs_batch_bound && (options.max_items.is_none() || options.max_bytes.is_none()) {
        return Err(syn::Error::new_spanned(
            field,
            "this runtime input requires both max_items and max_bytes",
        ));
    }
    if matches!(kind, InputKind::Read | InputKind::Request) && options.max_bytes.is_none() {
        return Err(syn::Error::new_spanned(
            field,
            "this runtime input requires max_response_bytes",
        ));
    }
    if kind == InputKind::Commands && options.port.is_none() {
        return Err(syn::Error::new_spanned(
            field,
            "Commands requires port = public::CONSTANT",
        ));
    }
    if kind != InputKind::Commands && options.port.is_some() {
        return Err(syn::Error::new_spanned(
            field,
            "port is allowed only on a Commands input",
        ));
    }
    Ok(())
}

fn input_type_check(ty: &Type, field: &Ident) -> TokenStream {
    let name = format_ident!("__phoxal_input_type_{}", field);
    quote! {
        fn #name() {
            fn check<T: ::phoxal::runtime::input::InputSpec>() {}
            check::<#ty>();
        }
    }
}

fn generic_types(ty: &Type, expected: usize) -> syn::Result<(Type, Type)> {
    let Type::Path(path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "runtime input type must be a generic path",
        ));
    };
    let segment = path
        .path
        .segments
        .last()
        .ok_or_else(|| syn::Error::new_spanned(ty, "missing type segment"))?;
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            ty,
            "runtime input type is missing its generic payloads",
        ));
    };
    let types = arguments
        .args
        .iter()
        .filter_map(|argument| match argument {
            GenericArgument::Type(ty) => Some(ty.clone()),
            _ => None,
        })
        .collect::<Vec<_>>();
    if types.len() != expected {
        return Err(syn::Error::new_spanned(
            ty,
            format!("runtime input expects {expected} type parameters"),
        ));
    }
    Ok((types[0].clone(), types[1].clone()))
}

fn option_tokens(value: Option<u64>) -> TokenStream {
    value.map_or_else(|| quote!(None), |value| quote!(Some(#value)))
}

/// Produces the shared type-level identity used to connect an input field to
/// an activation or reply collector in another declaration.
pub(crate) fn field_id(name: &Ident) -> u64 {
    let mut hash = 0xcbf29ce484222325_u64;
    for byte in name.to_string().bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3_u64);
    }
    hash
}
