//! Configuration schema derivation for the Runtime authoring surface.

use proc_macro2::{Span, TokenStream};
use quote::quote;
use syn::{Data, DeriveInput, Fields, LitStr, Type};

/// Derive `phoxal::runtime::ConfigSchema` from a named struct using the same
/// Serde attributes accepted by the Runtime configuration decoder.
pub fn expand_config(input: TokenStream) -> syn::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;
    let struct_name = &input.ident;

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            "#[derive(phoxal::Config)] does not support generic structs",
        ));
    }
    validate_config_serde_attributes(&input)?;

    let cx = serde_derive_internals::Ctxt::new();
    let container = serde_derive_internals::ast::Container::from_ast(
        &cx,
        &input,
        serde_derive_internals::Derive::Deserialize,
    );
    cx.check()?;
    let container = container.ok_or_else(|| {
        syn::Error::new_spanned(&input, "unable to parse Config with Serde's derive model")
    })?;

    let serde_derive_internals::ast::Data::Struct(
        serde_derive_internals::ast::Style::Struct,
        fields,
    ) = &container.data
    else {
        return Err(syn::Error::new_spanned(
            &input,
            "#[derive(phoxal::Config)] supports only structs with named fields",
        ));
    };

    let phoxal = quote!(::phoxal);
    let json_lit = |fragment: &str| {
        let lit = LitStr::new(fragment, Span::call_site());
        quote!(#lit)
    };
    let title = container.attrs.name().deserialize_name();
    let mut schema_args = vec![json_lit(&format!(
        "{{\"$schema\":\"https://json-schema.org/draft/2020-12/schema\",\"title\":{},\"type\":\"object\",\"properties\":{{",
        serde_json_string(title)
    ))];
    let mut required = Vec::new();
    for (index, field) in fields.iter().enumerate() {
        if index > 0 {
            schema_args.push(json_lit(","));
        }
        let field_name = field.attrs.name().deserialize_name();
        schema_args.push(json_lit(&format!("{}:", serde_json_string(field_name))));
        let ty = field.ty;
        schema_args.push(quote!(<#ty as #phoxal::runtime::ConfigSchema>::SCHEMA_JSON));

        if field.attrs.default().is_none() && !is_option_type(ty) {
            required.push(field_name.to_string());
        }
    }
    schema_args.push(json_lit("}"));
    if !required.is_empty() {
        let required_json = required
            .iter()
            .map(|name| serde_json_string(name))
            .collect::<Vec<_>>()
            .join(",");
        schema_args.push(json_lit(&format!(",\"required\":[{required_json}]")));
    }
    if container.attrs.deny_unknown_fields() {
        schema_args.push(json_lit(",\"additionalProperties\":false"));
    }
    schema_args.push(json_lit("}"));

    Ok(quote! {
        impl #phoxal::runtime::ConfigSchema for #struct_name {
            const __SCHEMA: #phoxal::runtime::ConfigSchemaValue =
                #phoxal::runtime::ConfigSchemaValue::new()
                    #(.push_str(#schema_args))*;
        }
    })
}

fn is_option_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(path)
        if path
            .path
            .segments
            .last()
            .is_some_and(|segment| segment.ident == "Option"))
}

fn serde_json_string(value: &str) -> String {
    let mut out = String::with_capacity(value.len() + 2);
    out.push('"');
    for ch in value.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            ch if ch <= '\u{1f}' => out.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => out.push(ch),
        }
    }
    out.push('"');
    out
}

fn validate_config_serde_attributes(input: &DeriveInput) -> syn::Result<()> {
    SerdeAttrLocation::Container.validate(&input.attrs)?;
    let Data::Struct(data) = &input.data else {
        return Err(syn::Error::new_spanned(
            input,
            "#[derive(phoxal::Config)] supports only structs with named fields",
        ));
    };
    if !matches!(data.fields, Fields::Named(_)) {
        return Err(syn::Error::new_spanned(
            &data.fields,
            "#[derive(phoxal::Config)] supports only structs with named fields",
        ));
    }
    for field in &data.fields {
        SerdeAttrLocation::Field.validate(&field.attrs)?;
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum SerdeAttrLocation {
    Container,
    Field,
}

impl SerdeAttrLocation {
    fn validate(self, attrs: &[syn::Attribute]) -> syn::Result<()> {
        for attr in attrs.iter().filter(|attr| attr.path().is_ident("serde")) {
            attr.parse_nested_meta(|meta| {
                let path = &meta.path;
                let name = meta
                    .path
                    .get_ident()
                    .map(ToString::to_string)
                    .unwrap_or_else(|| quote!(#path).to_string());
                let supported = match self {
                    Self::Container => matches!(
                        name.as_str(),
                        "rename" | "rename_all" | "default" | "deny_unknown_fields"
                    ),
                    Self::Field => matches!(name.as_str(), "rename" | "default"),
                };
                if !supported {
                    return Err(meta.error(format!(
                        "unsupported serde attribute `{name}` for #[derive(phoxal::Config)]; supported container attributes: rename, rename_all, default, deny_unknown_fields; supported field attributes: rename, default"
                    )));
                }

                match name.as_str() {
                    "rename" | "rename_all" => {
                        let _: LitStr = meta.value()?.parse()?;
                    }
                    "default" if meta.input.peek(syn::Token![=]) => {
                        let _: LitStr = meta.value()?.parse()?;
                    }
                    "default" | "deny_unknown_fields" if meta.input.is_empty() => {}
                    _ => {
                        return Err(meta.error(format!(
                            "unsupported form of serde attribute `{name}` for #[derive(phoxal::Config)]"
                        )));
                    }
                }
                Ok(())
            })?;
        }
        Ok(())
    }
}
