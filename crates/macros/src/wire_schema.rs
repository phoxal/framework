//! `#[derive(DescribeWire)]`: the serialized wire shape, read off the same
//! `#[serde(...)]` model `Serialize` itself reads.
//!
//! The derive computes what a `Serializer` is actually told to write - the map
//! keys after `rename`/`rename_all`, the tag style, the element order - and
//! nothing else. Serde's own attribute parser (`serde_derive_internals`)
//! resolves the names, so a rename rule cannot mean one thing here and another
//! thing on the wire.
//!
//! Anything whose serialized shape this cannot compute is a compile error
//! naming the remedy, never an approximation: a type with no derivable shape
//! gets a hand-written `DescribeWire` beside its custom serializer instead.

use proc_macro2::TokenStream;
use quote::quote;
use serde_derive_internals::ast::{Container, Data, Field, Style, Variant};
use serde_derive_internals::attr::TagType;
use serde_derive_internals::{Ctxt, Derive};
use syn::{DeriveInput, Type};

/// Where a hand-written implementation belongs, repeated on every rejection so
/// the diagnostic always ends with the way forward.
const REMEDY: &str = "hand-implement \
     `phoxal::__compat::wire::DescribeWire` beside this type's \
     custom serializer and declare the wire form it writes";

/// The path every generated impl reaches the model through.
///
/// The model lives on the process-contract floor, below every crate that owns
/// a wire contract, so the derive names it directly rather than routing through
/// the engine facade the authoring macros use: `phoxal-protocol` sits *below*
/// `phoxal` and could not resolve `::phoxal`.
fn model_path() -> TokenStream {
    quote!(::phoxal::__compat::wire)
}

/// Derive the serialized wire shape from a type's own declaration.
pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let input: DeriveInput = syn::parse2(input)?;

    if !input.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &input.generics,
            format!(
                "#[derive(DescribeWire)] does not support generic types: one declaration \
                 would stand for a family of different wire shapes; {REMEDY}"
            ),
        ));
    }

    let context = Ctxt::new();
    // `Derive::Serialize` on purpose: this model describes the shape a value
    // puts on the wire, so the serialize-side names and rules are the ones that
    // decide it.
    let container = Container::from_ast(&context, &input, Derive::Serialize);
    context.check()?;
    let container = container.ok_or_else(|| {
        syn::Error::new_spanned(&input, "unable to read this type with serde's derive model")
    })?;

    let body = container_schema(&container)?;
    let name = &input.ident;
    let model = model_path();
    Ok(quote! {
        #[automatically_derived]
        impl #model::DescribeWire for #name {
            fn wire_schema() -> #model::WireSchema {
                #body
            }
        }
    })
}

fn container_schema(container: &Container<'_>) -> syn::Result<TokenStream> {
    let model = model_path();
    let original = container.original;

    if container.attrs.has_flatten() {
        return Err(syn::Error::new_spanned(
            original,
            format!(
                "#[derive(DescribeWire)] does not support `#[serde(flatten)]`: the flattened \
                 fields are decided by another type's declaration, so this one does not state \
                 its own wire shape; {REMEDY}"
            ),
        ));
    }
    if let Some(remote) = container.attrs.remote() {
        return Err(syn::Error::new_spanned(
            remote,
            format!("#[derive(DescribeWire)] does not support `#[serde(remote)]`; {REMEDY}"),
        ));
    }

    // `into` replaces the serialized form outright, so the named type owns the
    // shape. `from`/`try_from` only narrow what a decoder accepts and are
    // deliberately not a shape fact.
    if let Some(into) = container.attrs.type_into() {
        return Ok(quote!(<#into as #model::DescribeWire>::wire_schema()));
    }

    // `#[serde(transparent)]` writes exactly the one field's value, with no
    // wrapping at all.
    if container.attrs.transparent() {
        let field = container
            .data
            .all_fields()
            .find(|field| field.attrs.transparent())
            .ok_or_else(|| {
                syn::Error::new_spanned(
                    original,
                    "#[serde(transparent)] names no serialized field on this type",
                )
            })?;
        return field_schema(field);
    }

    match &container.data {
        // A container-level `#[serde(default)]` reaches every field: each one
        // decodes from an absent key.
        Data::Struct(style, fields) => {
            struct_schema(*style, fields, !container.attrs.default().is_none())
        }
        Data::Enum(variants) => enum_schema(container, variants),
    }
}

fn struct_schema(
    style: Style,
    fields: &[Field<'_>],
    container_default: bool,
) -> syn::Result<TokenStream> {
    let model = model_path();
    match style {
        Style::Unit => Ok(quote!(#model::WireSchema::Unit)),
        Style::Newtype | Style::Tuple => {
            let items = fields
                .iter()
                .map(field_schema)
                .collect::<syn::Result<Vec<_>>>()?;
            // Serde gives a newtype struct exactly one field and writes it with
            // no wrapping; a tuple struct becomes a positional sequence.
            Ok(match items.as_slice() {
                [inner] if matches!(style, Style::Newtype) => {
                    quote!(#model::WireSchema::newtype(#inner))
                }
                items => quote!(#model::WireSchema::Tuple(::std::vec![#(#items),*])),
            })
        }
        Style::Struct => {
            let declared = named_fields(fields, container_default)?;
            Ok(quote!(#model::WireSchema::structure(::std::vec![#(#declared),*])))
        }
    }
}

fn enum_schema(container: &Container<'_>, variants: &[Variant<'_>]) -> syn::Result<TokenStream> {
    let model = model_path();
    let original = container.original;
    let representation = match container.attrs.tag() {
        TagType::External => quote!(#model::EnumRepresentation::ExternallyTagged),
        TagType::Internal { tag } => {
            quote!(#model::EnumRepresentation::InternallyTagged { tag: ::std::string::String::from(#tag) })
        }
        TagType::None => quote!(#model::EnumRepresentation::Untagged),
        TagType::Adjacent { .. } => {
            return Err(syn::Error::new_spanned(
                original,
                format!(
                    "#[derive(DescribeWire)] does not support adjacently tagged enums \
                     (`#[serde(tag = \"…\", content = \"…\")]`); {REMEDY}"
                ),
            ));
        }
    };

    let internally_tagged = matches!(container.attrs.tag(), TagType::Internal { .. });
    let mut declared = Vec::new();
    for variant in variants {
        if variant.attrs.skip_serializing() {
            return Err(syn::Error::new_spanned(
                variant.original,
                format!(
                    "#[derive(DescribeWire)] does not support `#[serde(skip)]` on a wire \
                     variant: a variant that is not written is not part of the shape this \
                     type declares; {REMEDY}"
                ),
            ));
        }
        if let Some(with) = variant.attrs.serialize_with() {
            return Err(syn::Error::new_spanned(
                with,
                format!(
                    "#[derive(DescribeWire)] does not support `serialize_with` on a variant: \
                     the shape it writes is not readable from this declaration; {REMEDY}"
                ),
            ));
        }

        let name = variant.attrs.name().serialize_name();
        let body = if variant.attrs.other() {
            quote!(#model::VariantBody::Other)
        } else {
            variant_body(variant, internally_tagged)?
        };
        declared.push(quote!(#model::WireVariant::new(#name, #body)));
    }

    Ok(quote! {
        #model::WireSchema::enumeration(#representation, ::std::vec![#(#declared),*])
    })
}

fn variant_body(variant: &Variant<'_>, internally_tagged: bool) -> syn::Result<TokenStream> {
    let model = model_path();
    match variant.style {
        Style::Unit => Ok(quote!(#model::VariantBody::Unit)),
        Style::Newtype => {
            // Serde gives a newtype variant exactly one field.
            let inner = variant
                .fields
                .first()
                .map(field_schema)
                .transpose()?
                .ok_or_else(|| {
                    syn::Error::new_spanned(variant.original, "a newtype variant carries one field")
                })?;
            Ok(quote!(#model::VariantBody::newtype(#inner)))
        }
        Style::Tuple => {
            if internally_tagged {
                return Err(syn::Error::new_spanned(
                    variant.original,
                    "an internally tagged enum cannot carry a tuple variant: there is no map \
                     for serde to merge the tag into",
                ));
            }
            let items = variant
                .fields
                .iter()
                .map(field_schema)
                .collect::<syn::Result<Vec<_>>>()?;
            Ok(quote!(#model::VariantBody::Tuple(::std::vec![#(#items),*])))
        }
        Style::Struct => {
            let declared = named_fields(&variant.fields, false)?;
            Ok(quote!(#model::VariantBody::structure(::std::vec![#(#declared),*])))
        }
    }
}

fn named_fields(fields: &[Field<'_>], container_default: bool) -> syn::Result<Vec<TokenStream>> {
    let model = model_path();
    let mut declared = Vec::new();
    for field in fields {
        let name = field.attrs.name().serialize_name();
        let schema = field_schema(field)?;
        // Both halves are wire facts: `default` (and an `Option`, which serde
        // defaults implicitly) says an absent field still decodes, while
        // `skip_serializing_if` says the writer may leave it out.
        let defaulted =
            container_default || !field.attrs.default().is_none() || is_option_type(field.ty);
        let omissible = field.attrs.skip_serializing_if().is_some();
        declared.push(quote! {
            #model::WireField::new(
                #name,
                #schema,
                #model::FieldPresence::new(#defaulted, #omissible),
            )
        });
    }
    Ok(declared)
}

/// The shape one field's value contributes.
fn field_schema(field: &Field<'_>) -> syn::Result<TokenStream> {
    let model = model_path();
    let original = field.original;

    if field.attrs.flatten() {
        return Err(syn::Error::new_spanned(
            original,
            format!("#[derive(DescribeWire)] does not support `#[serde(flatten)]`; {REMEDY}"),
        ));
    }
    if field.attrs.skip_serializing() || field.attrs.skip_deserializing() {
        return Err(syn::Error::new_spanned(
            original,
            format!(
                "#[derive(DescribeWire)] does not support `#[serde(skip)]` on a wire type: a \
                 field skipped on one side only is a shape the two sides disagree about; {REMEDY}"
            ),
        ));
    }
    if let Some(getter) = field.attrs.getter() {
        return Err(syn::Error::new_spanned(
            getter,
            format!("#[derive(DescribeWire)] does not support `#[serde(getter)]`; {REMEDY}"),
        ));
    }

    if let Some(with) = field.attrs.serialize_with() {
        // `#[serde(with = "serde_bytes")]` is the one recognized rewrite: it
        // writes a byte string, which the model names exactly. Every other
        // serializer is an arbitrary function whose output shape this
        // declaration does not state.
        return if is_serde_bytes(with) {
            Ok(quote!(#model::WireSchema::Bytes))
        } else {
            Err(syn::Error::new_spanned(
                with,
                format!(
                    "#[derive(DescribeWire)] does not support `serialize_with`/`with` on a \
                     field: the shape that function writes is not readable from this \
                     declaration; {REMEDY}"
                ),
            ))
        };
    }

    // A field's `deserialize_with` narrows which wire values decode - a finite
    // float, a bounded string - and never changes what is written, so it is not
    // a shape fact and is deliberately not recorded.
    let ty = field.ty;
    Ok(quote!(<#ty as #model::DescribeWire>::wire_schema()))
}

/// Whether a resolved `serialize_with` path is `serde_bytes`' own writer, which
/// `#[serde(with = "serde_bytes")]` expands to.
fn is_serde_bytes(path: &syn::ExprPath) -> bool {
    let segments = path
        .path
        .segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>();
    segments == ["serde_bytes", "serialize"]
}

/// Whether a field's declared type is an `Option`, which serde's derive decodes
/// as `None` when the field is absent even without `#[serde(default)]`.
///
/// This reads the written type, so an alias for `Option` would be missed. The
/// workspace writes `Option<T>` directly, and a wrong answer here is a wrong
/// presence rather than a wrong shape.
fn is_option_type(ty: &Type) -> bool {
    matches!(ty, Type::Path(path)
        if path.qself.is_none()
            && path
                .path
                .segments
                .last()
                .is_some_and(|segment| segment.ident == "Option"))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn expanded(input: TokenStream) -> String {
        expand(input)
            .expect("the declaration derives")
            .to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    fn rejected(input: TokenStream) -> String {
        expand(input)
            .expect_err("the declaration must be rejected")
            .to_string()
    }

    #[test]
    fn a_named_struct_declares_its_serialized_field_names() {
        let expansion = expanded(quote! {
            #[serde(rename_all = "camelCase", deny_unknown_fields)]
            struct Sample {
                unix_seconds: i64,
                #[serde(rename = "ns")]
                nanos: u32,
                detail: Option<String>,
                #[serde(default, skip_serializing_if = "Option::is_none")]
                label: Option<String>,
            }
        });
        assert!(
            expansion.contains(r#"WireField :: new ("unixSeconds""#),
            "{expansion}"
        );
        assert!(
            expansion.contains(r#"WireField :: new ("ns""#),
            "{expansion}"
        );
        // A plain `Option` decodes from an absent field; adding
        // `skip_serializing_if` also lets the writer omit it.
        assert!(
            expansion.contains(r#"WireField :: new ("detail" , < Option < String > as :: phoxal :: __compat :: wire :: DescribeWire > :: wire_schema () , :: phoxal :: __compat :: wire :: FieldPresence :: new (true , false) ,)"#),
            "{expansion}"
        );
        assert!(
            expansion.contains(r#"FieldPresence :: new (true , true)"#),
            "{expansion}"
        );
        // `deny_unknown_fields` is a parse rule, not a shape, so nothing in the
        // expansion mentions it.
        assert!(!expansion.contains("deny_unknown"), "{expansion}");
    }

    #[test]
    fn a_container_level_default_makes_every_field_decodable_while_absent() {
        let expansion = expanded(quote! {
            #[serde(default)]
            struct Sample {
                count: u32,
            }
        });
        assert!(
            expansion.contains("FieldPresence :: new (true , false)"),
            "{expansion}"
        );
    }

    #[test]
    fn the_struct_shapes_map_onto_serdes_own_forms() {
        assert!(expanded(quote! { struct Marker; }).contains("WireSchema :: Unit"));
        assert!(
            expanded(quote! { struct Wrapper(String); }).contains("WireSchema :: newtype"),
            "a newtype struct is written transparently"
        );
        assert!(
            expanded(quote! { struct Pair(u8, bool); }).contains("WireSchema :: Tuple"),
            "a tuple struct is written as a sequence"
        );
        assert!(
            expanded(quote! {
                #[serde(transparent)]
                struct Label(String);
            })
            .contains("< String as :: phoxal :: __compat :: wire :: DescribeWire >"),
            "a transparent struct writes exactly its one field"
        );
    }

    #[test]
    fn every_supported_enum_representation_is_read_from_the_serde_attributes() {
        let external = expanded(quote! {
            #[serde(rename_all = "snake_case")]
            enum Reason {
                TargetStale,
                Fault { code: i32 },
            }
        });
        assert!(
            external.contains("EnumRepresentation :: ExternallyTagged"),
            "{external}"
        );
        assert!(
            external.contains(r#"WireVariant :: new ("target_stale""#),
            "{external}"
        );

        let internal = expanded(quote! {
            #[serde(tag = "schema")]
            enum Document {
                #[serde(rename = "phoxal/example/v0")]
                V0 { value: u8 },
            }
        });
        assert!(
            internal.contains(
                r#"InternallyTagged { tag : :: std :: string :: String :: from ("schema") }"#
            ),
            "{internal}"
        );
        assert!(internal.contains(r#""phoxal/example/v0""#), "{internal}");

        let untagged = expanded(quote! {
            #[serde(untagged)]
            enum Value {
                Bool(bool),
                Text(String),
            }
        });
        assert!(
            untagged.contains("EnumRepresentation :: Untagged"),
            "{untagged}"
        );
    }

    /// `#[serde(other)]` is a decode fallback that removing would turn into a
    /// hard failure, so it is part of the shape rather than an implementation
    /// detail.
    #[test]
    fn an_other_variant_is_recorded_as_the_unknown_fallback() {
        let expansion = expanded(quote! {
            #[serde(rename_all = "snake_case")]
            enum Rejection {
                Busy,
                #[serde(other)]
                Unknown,
            }
        });
        assert!(expansion.contains("VariantBody :: Other"), "{expansion}");
    }

    /// `into` replaces the serialized form, so the named type owns the shape.
    /// `try_from` only narrows what decodes, so it leaves the shape alone.
    #[test]
    fn a_conversion_attribute_is_read_for_the_side_it_actually_changes() {
        let into = expanded(quote! {
            #[serde(try_from = "String", into = "String")]
            struct Reference(String);
        });
        assert!(
            into.contains(
                "< String as :: phoxal :: __compat :: wire :: DescribeWire > :: wire_schema ()"
            ),
            "{into}"
        );

        let try_from_only = expanded(quote! {
            #[serde(try_from = "StateWire")]
            struct State {
                x_m: f64,
            }
        });
        assert!(
            try_from_only.contains(r#"WireField :: new ("x_m""#),
            "{try_from_only}"
        );
        assert!(!try_from_only.contains("StateWire"), "{try_from_only}");
    }

    #[test]
    fn a_serde_bytes_field_is_the_one_recognized_serializer_rewrite() {
        let expansion = expanded(quote! {
            struct Frame {
                #[serde(with = "serde_bytes")]
                data: Vec<u8>,
            }
        });
        assert!(expansion.contains("WireSchema :: Bytes"), "{expansion}");
    }

    #[test]
    fn every_unmodelable_serde_feature_is_rejected_with_the_remedy() {
        let cases = [
            (
                quote! {
                    struct Outer {
                        #[serde(flatten)]
                        inner: Inner,
                    }
                },
                "flatten",
            ),
            (
                quote! {
                    #[serde(tag = "t", content = "c")]
                    enum Split {
                        A(u8),
                    }
                },
                "adjacently tagged",
            ),
            (
                quote! {
                    struct Custom {
                        #[serde(serialize_with = "write_it")]
                        value: u32,
                    }
                },
                "serialize_with",
            ),
            (
                quote! {
                    struct Hidden {
                        #[serde(skip)]
                        value: u32,
                    }
                },
                "skip",
            ),
            (
                quote! {
                    struct Generic<T> {
                        value: T,
                    }
                },
                "generic types",
            ),
        ];
        for (input, expected) in cases {
            let error = rejected(input);
            assert!(error.contains(expected), "{error}");
            assert!(error.contains("hand-implement"), "{error}");
        }
    }

    /// An unsupported `rename_all` style is serde's own error, reported against
    /// the attribute the author wrote, so the derive never invents a second
    /// vocabulary for the same rule.
    #[test]
    fn an_unknown_rename_rule_is_rejected_by_serdes_own_parser() {
        let error = rejected(quote! {
            #[serde(rename_all = "TitleCase")]
            struct Sample {
                value: u32,
            }
        });
        assert!(error.contains("unknown rename rule"), "{error}");
    }
}
