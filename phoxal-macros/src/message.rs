//! Expansion for Rust-authored Protobuf messages and oneofs.
//!
//! One `#[phoxal::message]` struct or enumeration is the single authored
//! definition: the expansion adds the Prost codec derives from the declared
//! tags, the Protobuf name, and the retained schema record host tooling
//! assembles into standard descriptors.

use heck::{ToShoutySnakeCase, ToSnakeCase, ToUpperCamelCase};
use proc_macro2::{Span, TokenStream};
use quote::{format_ident, quote};
use syn::parse::Parser;
use syn::{Expr, ExprLit, Fields, Ident, ItemEnum, ItemStruct, Lit, LitInt, LitStr, Path, Type};

/// The wire kind of one scalar as Prost spells it in field attributes.
#[derive(Clone, Copy, PartialEq)]
enum Scalar {
    Double,
    Float,
    Int64,
    Uint64,
    Int32,
    Uint32,
    Bool,
    Str,
}

impl Scalar {
    fn from_ident(ident: &Ident) -> Option<Self> {
        match ident.to_string().as_str() {
            "f64" => Some(Self::Double),
            "f32" => Some(Self::Float),
            "i64" => Some(Self::Int64),
            "u64" => Some(Self::Uint64),
            "i32" => Some(Self::Int32),
            "u32" => Some(Self::Uint32),
            "bool" => Some(Self::Bool),
            "String" => Some(Self::Str),
            _ => None,
        }
    }

    fn schema_ident(self) -> Ident {
        format_ident!(
            "{}",
            match self {
                Self::Double => "Double",
                Self::Float => "Float",
                Self::Int64 => "Int64",
                Self::Uint64 => "Uint64",
                Self::Int32 => "Int32",
                Self::Uint32 => "Uint32",
                Self::Bool => "Bool",
                Self::Str => "String",
            }
        )
    }

    fn is_copy(self) -> bool {
        !matches!(self, Self::Str)
    }
}

/// The analyzed wire shape of one authored field or oneof variant.
enum FieldShape {
    Scalar(Scalar),
    OptionalScalar(Scalar),
    RepeatedScalar(Scalar),
    Bytes,
    OptionalBytes,
    /// A named definition used as a field: its Rust type decides the wire
    /// behavior through `PhoxalWire`, so message and enumeration fields
    /// share one shape without an authoring hint.
    Definition(Path),
    OptionalDefinition(Path),
    RepeatedDefinition(Path),
}

impl FieldShape {
    /// The schema record entry for this field.
    fn schema_field(&self, name: &str, number: u32) -> TokenStream {
        let (ty, label, oneof) = match self {
            Self::Scalar(scalar) => {
                let kind = scalar.schema_ident();
                (
                    quote!(::phoxal::schema::FieldType::#kind),
                    quote!(Singular),
                    quote!(None),
                )
            }
            Self::OptionalScalar(scalar) => {
                let kind = scalar.schema_ident();
                (
                    quote!(::phoxal::schema::FieldType::#kind),
                    quote!(Optional),
                    quote!(None),
                )
            }
            Self::RepeatedScalar(scalar) => {
                let kind = scalar.schema_ident();
                (
                    quote!(::phoxal::schema::FieldType::#kind),
                    quote!(Repeated),
                    quote!(None),
                )
            }
            Self::Bytes => (
                quote!(::phoxal::schema::FieldType::Bytes),
                quote!(Singular),
                quote!(None),
            ),
            Self::OptionalBytes => (
                quote!(::phoxal::schema::FieldType::Bytes),
                quote!(Optional),
                quote!(None),
            ),
            Self::Definition(path)
            | Self::OptionalDefinition(path)
            | Self::RepeatedDefinition(path) => {
                let path = anchored(path, 1);
                let label = match self {
                    Self::OptionalDefinition(_) => quote!(Optional),
                    Self::RepeatedDefinition(_) => quote!(Repeated),
                    _ => quote!(Singular),
                };
                // The definition's own codec declares whether containing
                // records render it as an enumeration or a message field.
                let reference = quote!(
                    if <#path as ::phoxal::schema::PhoxalWire>::IS_ENUM {
                        ::phoxal::schema::FieldType::Enum(
                            <#path as ::phoxal::schema::MessageSchema>::WIRE_NAME
                        )
                    } else {
                        ::phoxal::schema::FieldType::Message(
                            <#path as ::phoxal::schema::MessageSchema>::WIRE_NAME
                        )
                    }
                );
                (reference, label, quote!(None))
            }
        };
        quote!(::phoxal::schema::FieldRecord {
            number: #number,
            name: #name,
            ty: #ty,
            label: ::phoxal::schema::Label::#label,
            oneof: #oneof,
        })
    }

    /// Retention expressions for the definitions this field references.
    fn retention(&self) -> Vec<TokenStream> {
        match self {
            Self::Definition(path)
            | Self::OptionalDefinition(path)
            | Self::RepeatedDefinition(path) => {
                let path = anchored(path, 1);
                vec![quote!(<#path as ::phoxal::schema::MessageSchema>::retain_schema())]
            }
            _ => Vec::new(),
        }
    }
}

/// Re-anchors an authored field type for generated code one module below
/// the authored item.
///
/// Paths rooted at the crate or starting with `::` resolve as authored;
/// `self` and leading `super` spellings rebase through [`anchored`]'s
/// rules, so `self::Settings` resolves exactly like the bare `Settings`.
pub(crate) fn anchored_type(ty: &Type, depth: usize) -> Type {
    let Type::Path(type_path) = ty else {
        return ty.clone();
    };
    let rooted = type_path.path.leading_colon.is_some()
        || type_path
            .path
            .segments
            .first()
            .is_some_and(|segment| segment.ident == "crate");
    if rooted {
        return ty.clone();
    }
    // Primitive names resolve everywhere; anchoring them would fabricate
    // `super::f64`.
    if type_path.path.segments.len() == 1
        && type_path.path.segments.first().is_some_and(|segment| {
            Scalar::from_ident(&segment.ident).is_some() || segment.ident == "u8"
        })
    {
        return ty.clone();
    }
    // Prelude containers keep their own name and anchor their payload.
    if let Some(segment) = type_path.path.segments.first()
        && matches!(
            segment.ident.to_string().as_str(),
            "Option" | "Vec" | "String" | "Box" | "HashMap" | "BTreeMap" | "PhantomData"
        )
    {
        let mut anchored = type_path.clone();
        if let Some(last) = anchored.path.segments.last_mut()
            && let syn::PathArguments::AngleBracketed(arguments) = &mut last.arguments
        {
            for argument in arguments.args.iter_mut() {
                if let syn::GenericArgument::Type(inner) = argument {
                    *inner = anchored_type(inner, depth);
                }
            }
        }
        return syn::Type::Path(anchored);
    }
    // Plain and `self`/`super`-prefixed paths share one re-anchoring rule
    // with [`anchored`], instead of blind `super` prefixing that would
    // fabricate `super::self::Settings`.
    syn::Type::Path(syn::TypePath {
        qself: None,
        path: anchored(&type_path.path, depth),
    })
}

/// Re-anchors an authored path for use in generated code that sits `depth`
/// modules below the authored item.
///
/// Paths rooted at the crate or starting with `::` resolve unchanged.
/// `self` resolves through `depth` `super` steps; each leading `super` adds
/// one more step, so the generated code keeps the author's own scope under
/// any spelling.
pub(crate) fn anchored(path: &Path, depth: usize) -> Path {
    let rooted = path.leading_colon.is_some()
        || path
            .segments
            .first()
            .is_some_and(|segment| segment.ident == "crate");
    if rooted {
        return path.clone();
    }
    let mut rest = path.segments.iter();
    if rest
        .clone()
        .next()
        .is_some_and(|segment| segment.ident == "self")
    {
        rest.next();
    }
    let mut leading_supers = 0;
    while rest
        .clone()
        .next()
        .is_some_and(|segment| segment.ident == "super")
    {
        leading_supers += 1;
        rest.next();
    }
    let mut anchored = Path {
        leading_colon: None,
        segments: Default::default(),
    };
    for _ in 0..depth + leading_supers {
        anchored.segments.push(syn::PathSegment {
            ident: syn::Ident::new("super", proc_macro2::Span::call_site()),
            arguments: syn::PathArguments::None,
        });
    }
    anchored.segments.extend(rest.cloned());
    anchored
}

/// The package identity an authored message declares.
enum PackageSource {
    /// An authored Protobuf package.
    Authored(LitStr),
    /// A private input: the owner-qualified identity derived from the
    /// authored item's crate and module.
    Private,
}

/// The package and wire-name expressions of one expansion, plus the
/// root-level consts anchoring a private identity.
///
/// Private identities evaluate `module_path!()` at the authored item's own
/// module; the generated schema module references the anchored consts
/// through `super` because its own `module_path!()` would report the wrong
/// owner.
struct PackageForms {
    /// Package expression at the expansion root.
    package_root: TokenStream,
    /// Package expression inside the generated schema module.
    package_nested: TokenStream,
    /// Wire-name expression at the expansion root.
    wire_root: TokenStream,
    /// Wire-name expression inside the generated schema module.
    wire_nested: TokenStream,
    /// Consts emitted before the authored item.
    declaration: TokenStream,
}

impl PackageSource {
    fn forms(&self, schema_name: &LitStr, shouting: &str) -> PackageForms {
        match self {
            Self::Authored(literal) => {
                let wire = LitStr::new(
                    &format!("{}.{}", literal.value(), schema_name.value()),
                    literal.span(),
                );
                PackageForms {
                    package_root: quote!(#literal),
                    package_nested: quote!(#literal),
                    wire_root: quote!(#wire),
                    wire_nested: quote!(#wire),
                    declaration: quote!(),
                }
            }
            Self::Private => {
                let package_anchor = format_ident!("PHOXAL_PACKAGE_{}", shouting);
                let wire_anchor = format_ident!("PHOXAL_WIRE_{}", shouting);
                PackageForms {
                    package_root: quote!(#package_anchor),
                    package_nested: quote!(super::#package_anchor),
                    wire_root: quote!(#wire_anchor),
                    wire_nested: quote!(super::#wire_anchor),
                    declaration: quote! {
                        const #package_anchor: &'static str =
                            ::phoxal::phoxal_private_identity!(module_path!());
                        const #wire_anchor: &'static str =
                            ::phoxal::phoxal_private_wire!(module_path!(), #schema_name);
                    },
                }
            }
        }
    }
}

/// Options accepted by `#[phoxal::message]`.
struct MessageOptions {
    package: PackageSource,
    schema_name: Option<LitStr>,
}

/// Expands `#[phoxal::message(package = "...")]` on a struct or enum.
pub(crate) fn expand_message(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let mut package = None;
    let mut schema_name = None;
    syn::meta::parser(|meta| {
        if meta.path.is_ident("package") {
            package = Some(meta.value()?.parse()?);
            Ok(())
        } else if meta.path.is_ident("schema_name") {
            schema_name = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta
                .error("unsupported #[phoxal::message] option; expected package or schema_name"))
        }
    })
    .parse2(attr)?;
    let options = MessageOptions {
        package: package.map_or(PackageSource::Private, PackageSource::Authored),
        schema_name,
    };
    match syn::parse2::<syn::Item>(item)? {
        syn::Item::Struct(item) => expand_struct(options, item),
        syn::Item::Enum(item) => expand_enum(options, item),
        other => Err(syn::Error::new_spanned(
            other,
            "#[phoxal::message] must be placed on a struct or enum",
        )),
    }
}

/// The `#[phoxal(...)]` attributes declared on one field.
#[derive(Default)]
struct FieldMeta {
    tag: Option<u32>,
    oneof_tags: Option<Vec<u32>>,
}

fn field_meta(field: &syn::Field) -> syn::Result<FieldMeta> {
    let mut meta = FieldMeta::default();
    for attr in field
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("phoxal"))
    {
        attr.parse_nested_meta(|nested| {
            if nested.path.is_ident("tag") {
                let value: LitInt = nested.value()?.parse()?;
                let parsed = value.base10_parse::<u32>()?;
                if parsed == 0 {
                    return Err(nested.error("field numbers start at one"));
                }
                meta.tag = Some(parsed);
                Ok(())
            } else if nested.path.is_ident("oneof") {
                if nested.input.is_empty() || nested.input.peek(syn::Token![,]) {
                    meta.oneof_tags = Some(Vec::new());
                } else {
                    let value: LitStr = nested.value()?.parse()?;
                    meta.oneof_tags = Some(parse_tag_list(&value)?);
                }
                Ok(())
            } else if nested.path.is_ident("tags") {
                let value: LitStr = nested.value()?.parse()?;
                meta.oneof_tags = Some(parse_tag_list(&value)?);
                Ok(())
            } else {
                Err(nested.error("unsupported field option; expected tag or oneof"))
            }
        })?;
    }
    Ok(meta)
}

/// The `#[phoxal(...)]` attributes declared on one oneof variant.
#[derive(Default)]
struct VariantMeta {
    tag: Option<u32>,
    oneof_tags: Option<Vec<u32>>,
}

fn variant_meta(variant: &syn::Variant) -> syn::Result<VariantMeta> {
    let mut meta = VariantMeta::default();
    for attr in variant
        .attrs
        .iter()
        .filter(|attr| attr.path().is_ident("phoxal"))
    {
        attr.parse_nested_meta(|nested| {
            if nested.path.is_ident("tag") {
                let value: LitInt = nested.value()?.parse()?;
                let parsed = value.base10_parse::<u32>()?;
                if parsed == 0 {
                    return Err(nested.error("field numbers start at one"));
                }
                meta.tag = Some(parsed);
                Ok(())
            } else if nested.path.is_ident("oneof") || nested.path.is_ident("tags") {
                let value: LitStr = nested.value()?.parse()?;
                meta.oneof_tags = Some(parse_tag_list(&value)?);
                Ok(())
            } else {
                Err(nested.error("unsupported variant option; expected tag"))
            }
        })?;
    }
    Ok(meta)
}

fn parse_tag_list(tags: &LitStr) -> syn::Result<Vec<u32>> {
    let mut parsed = Vec::new();
    for part in tags.value().split(',') {
        let part = part.trim();
        let value: u32 = part.parse().map_err(|_| {
            syn::Error::new_spanned(tags, "oneof tags must be comma-separated numbers")
        })?;
        if value == 0 {
            return Err(syn::Error::new_spanned(tags, "field numbers start at one"));
        }
        parsed.push(value);
    }
    if parsed.is_empty() {
        return Err(syn::Error::new_spanned(
            tags,
            "a oneof needs at least one tag",
        ));
    }
    Ok(parsed)
}

/// One fully analyzed message field.
struct AnalyzedField {
    ident: Ident,
    shape: FieldShape,
    tag: u32,
}

fn analyze_field(field: &syn::Field) -> syn::Result<AnalyzedField> {
    let meta = field_meta(field)?;
    let ident = field
        .ident
        .clone()
        .ok_or_else(|| syn::Error::new_spanned(field, "message fields must be named"))?;
    let shape = analyze_type(&field.ty, &meta, &ident)?;
    let tag = meta.tag.ok_or_else(|| {
        syn::Error::new_spanned(
            &field.ty,
            "every field declares its number, e.g. #[phoxal(tag = 1)]",
        )
    })?;
    Ok(AnalyzedField { ident, shape, tag })
}

fn analyze_type(ty: &Type, meta: &FieldMeta, _ident: &Ident) -> syn::Result<FieldShape> {
    let Type::Path(type_path) = ty else {
        return Err(syn::Error::new_spanned(
            ty,
            "#[phoxal::message] supports named scalar, definition, Vec, and Option field types",
        ));
    };
    let segment = type_path.path.segments.last().ok_or_else(|| {
        syn::Error::new_spanned(ty, "#[phoxal::message] field type has no final segment")
    })?;
    let segment_ident = &segment.ident;
    if segment_ident == "Option" {
        let inner = single_generic_argument(segment, ty)?;
        if is_u8(&inner) {
            return Err(syn::Error::new_spanned(
                ty,
                "bytes fields are Vec<u8>, not Option<u8>",
            ));
        }
        if let Type::Path(inner_path_ty) = &inner
            && let Some(inner_segment) = inner_path_ty.path.segments.last()
            && inner_segment.ident == "Vec"
        {
            let byte_inner = single_generic_argument(inner_segment, ty)?;
            if is_u8(&byte_inner) {
                return Ok(FieldShape::OptionalBytes);
            }
            return Err(syn::Error::new_spanned(
                ty,
                "repeated fields cannot carry proto3 optional presence",
            ));
        }
        if let Some(scalar) = bare_scalar(&inner) {
            return Ok(FieldShape::OptionalScalar(scalar));
        }
        reject_generics(&inner_path(&inner, ty)?, ty)?;
        return Ok(FieldShape::OptionalDefinition(inner_path(&inner, ty)?));
    }
    if segment_ident == "Vec" {
        let inner = single_generic_argument(segment, ty)?;
        if meta.oneof_tags.is_some() {
            return Err(syn::Error::new_spanned(
                ty,
                "oneof fields are Option-valued; declare the oneof on its Option field",
            ));
        }
        if is_u8(&inner) {
            return Ok(FieldShape::Bytes);
        }
        if let Some(scalar) = bare_scalar(&inner) {
            return Ok(FieldShape::RepeatedScalar(scalar));
        }
        reject_generics(&inner_path(&inner, ty)?, ty)?;
        return Ok(FieldShape::RepeatedDefinition(inner_path(&inner, ty)?));
    }
    if meta.oneof_tags.is_some() {
        return Err(syn::Error::new_spanned(
            ty,
            "oneof fields are Option-valued; declare the oneof on its Option field",
        ));
    }
    if let Some(scalar) = Scalar::from_ident(segment_ident) {
        return Ok(FieldShape::Scalar(scalar));
    }
    reject_generics(&type_path.path, ty)?;
    Ok(FieldShape::Definition(type_path.path.clone()))
}

/// The scalar of a bare non-container inner type, if it names one.
fn bare_scalar(inner: &Type) -> Option<Scalar> {
    let Type::Path(type_path) = inner else {
        return None;
    };
    type_path
        .path
        .segments
        .last()
        .and_then(|segment| Scalar::from_ident(&segment.ident))
}

fn single_generic_argument(segment: &syn::PathSegment, outer: &Type) -> syn::Result<Type> {
    let syn::PathArguments::AngleBracketed(arguments) = &segment.arguments else {
        return Err(syn::Error::new_spanned(
            outer,
            "expected exactly one generic argument",
        ));
    };
    match arguments.args.len() {
        1 => match arguments.args.first() {
            Some(syn::GenericArgument::Type(ty)) => Ok(ty.clone()),
            _ => Err(syn::Error::new_spanned(
                outer,
                "expected exactly one generic type argument",
            )),
        },
        _ => Err(syn::Error::new_spanned(
            outer,
            "expected exactly one generic type argument",
        )),
    }
}

fn inner_path(ty: &Type, outer: &Type) -> syn::Result<Path> {
    let Type::Path(type_path) = ty else {
        return Err(syn::Error::new_spanned(
            outer,
            "expected a message or scalar path inside the generic argument",
        ));
    };
    Ok(type_path.path.clone())
}

fn is_u8(ty: &Type) -> bool {
    matches!(ty, Type::Path(type_path) if type_path.path.is_ident("u8"))
}

fn reject_generics(path: &Path, outer: &Type) -> syn::Result<()> {
    if path
        .segments
        .iter()
        .any(|segment| !segment.arguments.is_none())
    {
        return Err(syn::Error::new_spanned(
            outer,
            "#[phoxal::message] does not support generic field types",
        ));
    }
    Ok(())
}

fn expand_struct(options: MessageOptions, mut item: ItemStruct) -> syn::Result<TokenStream> {
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &item.generics,
            "#[phoxal::message] does not support generic messages",
        ));
    }
    let Fields::Named(fields) = &mut item.fields else {
        return Err(syn::Error::new_spanned(
            &item.fields,
            "#[phoxal::message] structs need named fields",
        ));
    };
    let struct_name = item.ident.clone();
    let schema_name = options
        .schema_name
        .clone()
        .unwrap_or_else(|| LitStr::new(&struct_name.to_string(), struct_name.span()));
    let mut analyzed = Vec::new();
    for field in &fields.named {
        analyzed.push(analyze_field(field)?);
    }
    let module = format_ident!("phoxal_schema_{}", struct_name.to_string().to_snake_case());
    let forms = options.package.forms(
        &schema_name,
        &struct_name.to_string().to_shouty_snake_case(),
    );
    let PackageForms {
        package_root,
        package_nested,
        wire_root,
        wire_nested,
        declaration: package_declaration,
    } = forms;

    // Every field encodes and decodes through its PhoxalWire
    // implementation: scalars, strings, bytes, definitions, and oneofs
    // share one path, so the generated Prost Message implementation needs
    // no per-kind dispatch and no wire mirror.
    let mut rebuilt_fields = Vec::new();
    let mut encode_stmts = Vec::new();
    let mut merge_arms = Vec::new();
    let mut len_exprs = Vec::new();
    let mut schema_fields = Vec::new();
    let mut retention = Vec::new();
    for (field, analysis) in fields.named.iter().zip(&analyzed) {
        let attrs: Vec<_> = field
            .attrs
            .iter()
            .filter(|attr| !attr.path().is_ident("phoxal"))
            .cloned()
            .collect();
        let vis = &field.vis;
        let ident = &analysis.ident;
        let ty = &field.ty;
        rebuilt_fields.push(quote! {
            #(#attrs)*
            #vis #ident: #ty,
        });
        let tag = analysis.tag;
        match &analysis.shape {
            FieldShape::RepeatedScalar(_) | FieldShape::RepeatedDefinition(_) => {
                encode_stmts.push(quote! {
                    ::phoxal::schema::encode_repeated(&self.#ident, #tag, buf);
                });
                merge_arms.push(quote! {
                    #tag => ::phoxal::schema::merge_repeated(
                        &mut self.#ident,
                        wire_type,
                        buf,
                        ctx,
                    ),
                });
                len_exprs.push(quote! {
                    + ::phoxal::schema::repeated_len(&self.#ident, #tag)
                });
            }

            _ => {
                // Implicit presence: a default-valued scalar, string,
                // bytes, or enumeration occurrence is omitted, matching
                // Prost's derived encoders; explicit `Option` fields and
                // message fields always encode.
                encode_stmts.push(quote! {
                    ::phoxal::schema::singular_encode(&self.#ident, #tag, buf);
                });
                merge_arms.push(quote! {
                    #tag => ::phoxal::schema::PhoxalWire::phoxal_merge(
                        &mut self.#ident,
                        #tag,
                        wire_type,
                        buf,
                        ctx,
                    ),
                });
                len_exprs.push(quote! {
                    + ::phoxal::schema::singular_len(&self.#ident, #tag)
                });
            }
        }
        let name = ident.to_string();
        schema_fields.push(analysis.shape.schema_field(&name, tag));
        retention.extend(analysis.shape.retention());
    }

    let vis = &item.vis;
    let struct_attrs: Vec<_> = item
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("phoxal"))
        .cloned()
        .collect();
    // Prost derives Copy for messages whose every field is copyable;
    // consumers rely on that convenience, so the same rule applies here.
    let all_copy = analyzed.iter().all(|field| {
        matches!(&field.shape, FieldShape::Scalar(scalar) | FieldShape::OptionalScalar(scalar) if scalar.is_copy())
    });
    let copy_derive = if all_copy { quote!(Copy,) } else { quote!() };

    let struct_derive = quote!(#[derive(Clone, #copy_derive Debug, PartialEq, Default)]);

    let message_impl = quote! {
        impl ::phoxal::generated::prost::Message for #struct_name {
            fn encode_raw(
                &self,
                buf: &mut impl ::phoxal::generated::prost::bytes::BufMut,
            ) {
                #(#encode_stmts)*
            }

            fn merge_field(
                &mut self,
                tag: u32,
                wire_type: ::phoxal::generated::prost::encoding::WireType,
                buf: &mut impl ::phoxal::generated::prost::bytes::Buf,
                ctx: ::phoxal::generated::prost::encoding::DecodeContext,
            ) -> ::std::result::Result<(), ::phoxal::generated::prost::DecodeError> {
                match tag {
                    #(#merge_arms)*
                    _ => ::phoxal::generated::prost::encoding::skip_field(
                        wire_type,
                        tag,
                        buf,
                        ctx,
                    ),
                }
            }

            fn encoded_len(&self) -> usize {
                0 #(#len_exprs)*
            }

            fn clear(&mut self) {
                *self = Self::default();
            }
        }

        impl ::phoxal::schema::PhoxalWire for #struct_name {
            const WIRE_TYPE: ::phoxal::generated::prost::encoding::WireType =
                ::phoxal::generated::prost::encoding::WireType::LengthDelimited;
            const IS_ENUM: bool = false;
            const SKIP_DEFAULT: bool = false;

            fn phoxal_write<B: ::phoxal::generated::prost::bytes::BufMut>(&self, buf: &mut B) {
                let len = <Self as ::phoxal::generated::prost::Message>::encoded_len(self);
                ::phoxal::generated::prost::encoding::encode_varint(len as u64, buf);
                <Self as ::phoxal::generated::prost::Message>::encode_raw(self, buf);
            }

            fn phoxal_value_len(&self) -> usize {
                let len = <Self as ::phoxal::generated::prost::Message>::encoded_len(self);
                ::phoxal::generated::prost::encoding::encoded_len_varint(len as u64) + len
            }

            fn phoxal_merge<B: ::phoxal::generated::prost::bytes::Buf>(
                &mut self,
                _tag: u32,
                wire_type: ::phoxal::generated::prost::encoding::WireType,
                buf: &mut B,
                ctx: ::phoxal::generated::prost::encoding::DecodeContext,
            ) -> ::std::result::Result<(), ::phoxal::generated::prost::DecodeError> {
                ::phoxal::generated::prost::encoding::message::merge(wire_type, self, buf, ctx)
            }

            fn phoxal_merge_fresh<B: ::phoxal::generated::prost::bytes::Buf>(
                tag: u32,
                wire_type: ::phoxal::generated::prost::encoding::WireType,
                buf: &mut B,
                ctx: ::phoxal::generated::prost::encoding::DecodeContext,
            ) -> ::std::result::Result<Self, ::phoxal::generated::prost::DecodeError> {
                ::phoxal::schema::merge_fresh_default(tag, wire_type, buf, ctx)
            }
        }

        impl ::phoxal::contracts::ProstPayload for #struct_name {
            type Wire = Self;

            fn to_wire(&self) -> Self {
                <Self as ::std::clone::Clone>::clone(self)
            }

            fn try_from_wire(
                wire: Self,
            ) -> ::std::result::Result<Self, ::phoxal::generated::prost::DecodeError> {
                ::std::result::Result::Ok(wire)
            }
        }
    };

    Ok(quote! {
        #package_declaration
        #(#struct_attrs)*
        #struct_derive
        #vis struct #struct_name {
            #(#rebuilt_fields)*
        }

        impl ::phoxal::generated::prost::Name for #struct_name {
            const NAME: &'static str = #schema_name;
            const PACKAGE: &'static str = #package_root;
            fn full_name() -> ::phoxal::generated::prost::alloc::string::String {
                #wire_root.into()
            }
            fn type_url() -> ::phoxal::generated::prost::alloc::string::String {
                let wire = #wire_root;
                let mut url = ::phoxal::generated::prost::alloc::string::String::with_capacity(
                    wire.len() + 1,
                );
                url.push('/');
                url.push_str(wire);
                url
            }
        }

        #message_impl

        mod #module {
            use super::#struct_name;

            const RECORD: ::phoxal::schema::SchemaRecord<'static> =
                ::phoxal::schema::SchemaRecord::Message(::phoxal::schema::MessageRecord {
                    package: #package_nested,
                    name: #schema_name,
                    fields: &[#(#schema_fields),*],
                });

            #[used]
            #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_schema"))]
            #[cfg_attr(target_os = "linux", unsafe(link_section = ".phoxal_schema"))]
            static FRAME: [u8; ::phoxal::schema::encoded_len(&RECORD)] = {
                let mut bytes = [0_u8; ::phoxal::schema::encoded_len(&RECORD)];
                ::phoxal::schema::write_frame(&RECORD, &mut bytes);
                bytes
            };

            impl ::phoxal::schema::MessageSchema for #struct_name {
                const RECORD: ::phoxal::schema::SchemaRecord<'static> = RECORD;
                const WIRE_NAME: &'static str = #wire_nested;

                fn retain_schema() -> usize {
                    let mut size = ::std::hint::black_box(&FRAME).len();
                    #(
                        size += #retention;
                    )*
                    size
                }
            }

        }
    })
}

/// One analyzed payload-enum variant.
struct PayloadVariant {
    ident: Ident,
    tag: u32,
    /// `None` for a unit variant; the payload type as authored at the
    /// expansion root for a tuple variant.
    payload: Option<Type>,
    /// The payload type as seen from inside the generated schema module.
    anchored_payload: Type,
    /// The internal empty message generated for a unit variant.
    unit_payload: Option<Ident>,
    /// Attributes the author placed on the variant besides the phoxal tag.
    attrs: Vec<syn::Attribute>,
    schema_record: TokenStream,
    retention: TokenStream,
}

/// Expands `#[phoxal::message]` on an enum whose variants carry explicit
/// `#[phoxal(tag = …)]` attributes: an ordinary Rust payload enum.
///
/// The public enum is the authoring surface and owns its Protobuf message
/// envelope; a private wire mirror inside the schema module carries the
/// internal oneof so decoding can reject envelopes that select no variant
/// instead of fabricating a default. Unit variants generate an internal
/// empty wire payload; tuple variants carry exactly one payload type.
fn expand_payload_enum(options: MessageOptions, item: ItemEnum) -> syn::Result<TokenStream> {
    let enum_name = item.ident.clone();
    let schema_name = options
        .schema_name
        .clone()
        .unwrap_or_else(|| LitStr::new(&enum_name.to_string(), enum_name.span()));
    let module = format_ident!("phoxal_schema_{}", enum_name.to_string().to_snake_case());
    let shouting_name = schema_name.value().to_shouty_snake_case();
    let PackageForms {
        package_nested,
        wire_nested,
        declaration: package_declaration,
        ..
    } = options.package.forms(&schema_name, &shouting_name);

    let mut variants = Vec::<PayloadVariant>::new();
    let mut all_copy = true;
    for variant in &item.variants {
        let meta = variant_meta(variant)?;
        if meta.oneof_tags.is_some() {
            return Err(syn::Error::new_spanned(
                variant,
                "a payload-enum variant cannot itself declare a oneof tag list",
            ));
        }
        let tag = meta.tag.ok_or_else(|| {
            syn::Error::new_spanned(
                variant,
                "every payload-enum variant declares its tag, e.g. #[phoxal(tag = 1)]",
            )
        })?;
        let name_snake = variant.ident.to_string().to_snake_case();
        let attrs: Vec<_> = variant
            .attrs
            .iter()
            .filter(|attr| !attr.path().is_ident("phoxal"))
            .cloned()
            .collect();
        let ident = variant.ident.clone();
        let payload_meta = FieldMeta {
            tag: meta.tag,
            oneof_tags: None,
        };
        let (payload, anchored_payload, unit_payload, schema_record, retention) = match &variant
            .fields
        {
            Fields::Unit => {
                let unit_ident = format_ident!("{}{}", enum_name, variant.ident);
                let unit_path: Type = syn::parse_quote!(#unit_ident);
                // The record and the unit payload share the schema module,
                // so the reference stays unanchored.
                let record = quote!(::phoxal::schema::FieldRecord {
                    number: #tag,
                    name: #name_snake,
                    ty: ::phoxal::schema::FieldType::Message(
                        <#unit_ident as ::phoxal::schema::MessageSchema>::WIRE_NAME,
                    ),
                    label: ::phoxal::schema::Label::Singular,
                    oneof: ::std::option::Option::None,
                });
                let retention =
                    quote!(<#unit_ident as ::phoxal::schema::MessageSchema>::retain_schema());
                (None, unit_path, Some(unit_ident), record, retention)
            }
            Fields::Unnamed(fields) => {
                if fields.unnamed.len() != 1 {
                    return Err(syn::Error::new_spanned(
                        variant,
                        "a payload-enum variant carries at most one payload",
                    ));
                }
                let payload = fields
                    .unnamed
                    .first()
                    .ok_or_else(|| {
                        syn::Error::new_spanned(variant, "a payload-enum variant needs a payload")
                    })?
                    .ty
                    .clone();
                let shape = analyze_type(&payload, &payload_meta, &variant.ident)?;
                all_copy &= matches!(&shape, FieldShape::Scalar(scalar) if scalar.is_copy());
                let record = shape.schema_field(&name_snake, tag);
                let retention = match shape.retention().is_empty() {
                    true => quote!(0),
                    false => {
                        let tokens = shape.retention();
                        quote!(#(#tokens)+*)
                    }
                };
                (
                    Some(payload.clone()),
                    anchored_type(&payload, 1),
                    None,
                    record,
                    retention,
                )
            }
            Fields::Named(_) => {
                return Err(syn::Error::new_spanned(
                    variant,
                    "payload-enum variants are unit variants or carry exactly one payload",
                ));
            }
        };
        variants.push(PayloadVariant {
            ident,
            tag,
            payload,
            anchored_payload,
            unit_payload,
            attrs,
            schema_record,
            retention,
        });
    }
    if variants.is_empty() {
        return Err(syn::Error::new_spanned(
            &item,
            "a payload enum declares at least one variant",
        ));
    }

    // Unit variants render without a payload field in the public enum.
    let mut public_variants = Vec::new();
    for variant in &variants {
        let ident = &variant.ident;
        let attrs = &variant.attrs;
        match &variant.payload {
            None => public_variants.push(quote! {
                #(#attrs)*
                #ident,
            }),
            Some(payload) => public_variants.push(quote! {
                #(#attrs)*
                #ident(#payload),
            }),
        }
    }

    // The wire mirror: the old wrapper-plus-oneof shape, private to the
    // schema module, carrying the merge semantics Prost's derived oneof
    // decoders implement.
    let mut selection_variants = Vec::new();
    let mut selection_encode_arms = Vec::new();
    let mut selection_write_arms = Vec::new();
    let mut selection_value_len_arms = Vec::new();
    let mut selection_merge_arms = Vec::new();
    let mut selection_fresh_arms = Vec::new();
    let mut selection_len_arms = Vec::new();
    let mut wire_merge_arms = Vec::new();
    let mut to_wire_arms = Vec::new();
    let mut from_wire_arms = Vec::new();
    let mut tags = Vec::new();
    let mut unit_payloads = Vec::new();
    for variant in &variants {
        let ident = &variant.ident;
        let tag = variant.tag;
        let anchored = &variant.anchored_payload;
        tags.push(tag);
        selection_variants.push(quote! { #ident(#anchored), });
        selection_encode_arms.push(quote! {
            Selection::#ident(value) =>
                ::phoxal::schema::PhoxalWire::phoxal_encode(value, #tag, buf),
        });
        selection_write_arms.push(quote! {
            Selection::#ident(value) =>
                ::phoxal::schema::PhoxalWire::phoxal_write(value, buf),
        });
        selection_value_len_arms.push(quote! {
            Selection::#ident(value) =>
                ::phoxal::schema::PhoxalWire::phoxal_value_len(value),
        });
        selection_len_arms.push(quote! {
            Selection::#ident(value) =>
                ::phoxal::schema::PhoxalWire::phoxal_encoded_len(value, #tag),
        });
        selection_merge_arms.push(quote! {
            #tag => {
                if let Selection::#ident(value) = self {
                    ::phoxal::schema::PhoxalWire::phoxal_merge(
                        value,
                        #tag,
                        wire_type,
                        buf,
                        ctx,
                    )?;
                } else {
                    let mut value = <#anchored as ::std::default::Default>::default();
                    ::phoxal::schema::PhoxalWire::phoxal_merge(
                        &mut value,
                        #tag,
                        wire_type,
                        buf,
                        ctx,
                    )?;
                    *self = Selection::#ident(value);
                }
            }
        });
        selection_fresh_arms.push(quote! {
            #tag => {
                let mut value = <#anchored as ::std::default::Default>::default();
                ::phoxal::schema::PhoxalWire::phoxal_merge(
                    &mut value,
                    #tag,
                    wire_type,
                    buf,
                    ctx,
                )?;
                ::std::result::Result::Ok(Selection::#ident(value))
            }
        });
        wire_merge_arms.push(quote! {
            #tag => ::phoxal::schema::PhoxalWire::phoxal_merge(
                &mut self.selection,
                #tag,
                wire_type,
                buf,
                ctx,
            ),
        });
        match (&variant.payload, &variant.unit_payload) {
            (None, Some(unit_ident)) => {
                to_wire_arms.push(quote! {
                    #enum_name::#ident =>
                        Wire { selection: ::std::option::Option::Some(Selection::#ident(#unit_ident)) },
                });
                from_wire_arms.push(quote! {
                    ::std::option::Option::Some(Selection::#ident(_)) =>
                        ::std::result::Result::Ok(#enum_name::#ident),
                });
            }
            (Some(_), None) => {
                to_wire_arms.push(quote! {
                    #enum_name::#ident(value) => Wire {
                        selection: ::std::option::Option::Some(
                            Selection::#ident(::std::clone::Clone::clone(value)),
                        ),
                    },
                });
                from_wire_arms.push(quote! {
                    ::std::option::Option::Some(Selection::#ident(value)) =>
                        ::std::result::Result::Ok(#enum_name::#ident(value)),
                });
            }
            _ => unreachable!("a variant is either unit or single-payload"),
        }
        if let Some(unit_ident) = &variant.unit_payload {
            let unit_wire_name = variant.unit_wire_name(&wire_nested);
            let unit_wire_name_tail = LitStr::new(
                &format!("{}{}", schema_name.value(), variant.ident),
                unit_ident.span(),
            );
            unit_payloads.push(quote! {
                #[derive(Clone, Copy, Debug, PartialEq, Default)]
                pub struct #unit_ident;

                impl ::phoxal::generated::prost::Message for #unit_ident {
                    fn encode_raw(
                        &self,
                        buf: &mut impl ::phoxal::generated::prost::bytes::BufMut,
                    ) {
                        let _ = buf;
                    }

                    fn merge_field(
                        &mut self,
                        tag: u32,
                        wire_type: ::phoxal::generated::prost::encoding::WireType,
                        buf: &mut impl ::phoxal::generated::prost::bytes::Buf,
                        ctx: ::phoxal::generated::prost::encoding::DecodeContext,
                    ) -> ::std::result::Result<(), ::phoxal::generated::prost::DecodeError> {
                        ::phoxal::generated::prost::encoding::skip_field(
                            wire_type,
                            tag,
                            buf,
                            ctx,
                        )
                    }

                    fn encoded_len(&self) -> usize {
                        0
                    }

                    fn clear(&mut self) {}
                }

                impl ::phoxal::schema::PhoxalWire for #unit_ident {
                    const WIRE_TYPE: ::phoxal::generated::prost::encoding::WireType =
                        ::phoxal::generated::prost::encoding::WireType::LengthDelimited;
                    const IS_ENUM: bool = false;
                    const SKIP_DEFAULT: bool = false;

                    fn phoxal_write<B: ::phoxal::generated::prost::bytes::BufMut>(
                        &self,
                        buf: &mut B,
                    ) {
                        ::phoxal::generated::prost::encoding::encode_varint(0_u64, buf);
                    }

                    fn phoxal_value_len(&self) -> usize {
                        1
                    }

                    fn phoxal_merge<B: ::phoxal::generated::prost::bytes::Buf>(
                        &mut self,
                        _tag: u32,
                        wire_type: ::phoxal::generated::prost::encoding::WireType,
                        buf: &mut B,
                        ctx: ::phoxal::generated::prost::encoding::DecodeContext,
                    ) -> ::std::result::Result<(), ::phoxal::generated::prost::DecodeError> {
                        ::phoxal::generated::prost::encoding::message::merge(
                            wire_type,
                            self,
                            buf,
                            ctx,
                        )
                    }

                    fn phoxal_merge_fresh<B: ::phoxal::generated::prost::bytes::Buf>(
                        tag: u32,
                        wire_type: ::phoxal::generated::prost::encoding::WireType,
                        buf: &mut B,
                        ctx: ::phoxal::generated::prost::encoding::DecodeContext,
                    ) -> ::std::result::Result<Self, ::phoxal::generated::prost::DecodeError> {
                        ::phoxal::schema::merge_fresh_default(tag, wire_type, buf, ctx)
                    }
                }

                // Each unit payload's schema frame lives in its own unnamed
                // const scope: the frame's fixed name would otherwise collide
                // for a payload enum with more than one unit variant.
                const _: () = {
                    impl ::phoxal::schema::MessageSchema for #unit_ident {
                        const RECORD: ::phoxal::schema::SchemaRecord<'static> =
                            ::phoxal::schema::SchemaRecord::Message(
                                ::phoxal::schema::MessageRecord {
                                    package: #package_nested,
                                    name: #unit_wire_name_tail,
                                    fields: &[],
                                },
                            );
                        const WIRE_NAME: &'static str = #unit_wire_name;

                        fn retain_schema() -> usize {
                            ::std::hint::black_box(&UNIT_FRAME).len()
                        }
                    }

                    #[used]
                    #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_schema"))]
                    #[cfg_attr(target_os = "linux", unsafe(link_section = ".phoxal_schema"))]
                    static UNIT_FRAME: [u8; ::phoxal::schema::encoded_len(
                        &<#unit_ident as ::phoxal::schema::MessageSchema>::RECORD,
                    )] = {
                        let mut bytes = [0_u8; ::phoxal::schema::encoded_len(
                            &<#unit_ident as ::phoxal::schema::MessageSchema>::RECORD,
                        )];
                        ::phoxal::schema::write_frame(
                            &<#unit_ident as ::phoxal::schema::MessageSchema>::RECORD,
                            &mut bytes,
                        );
                        bytes
                    };
                };
            });
        }
    }
    let tag_list = quote!(&[#(#tags),*]);
    let first_tag = variants[0].tag;
    // Mirrors `phoxal::schema::PAYLOAD_ENUM_ENVELOPE`; the macros crate
    // cannot import the SDK, and the build helper carries the same name to
    // rebuild payload enums for robot clients.
    let envelope_lit = LitStr::new("phoxal_envelope", proc_macro2::Span::call_site());
    let variant_records: Vec<_> = variants.iter().map(|v| &v.schema_record).collect();

    let vis = &item.vis;
    let enum_attrs: Vec<_> = item
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("phoxal"))
        .cloned()
        .collect();
    let copy_derive = if all_copy { quote!(Copy,) } else { quote!() };

    let variant_retention: Vec<_> = variants.iter().map(|v| &v.retention).collect();

    Ok(quote! {
        #package_declaration
        #(#enum_attrs)*
        #[derive(Clone, #copy_derive Debug, PartialEq)]
        #vis enum #enum_name {
            #(#public_variants)*
        }

        mod #module {
            use super::#enum_name;

            /// The private Prost wire form: the internal oneof envelope.
            #[derive(Clone, Debug, PartialEq, Default)]
            pub struct Wire {
                /// The selected variant, absent while decoding.
                pub selection: ::std::option::Option<Selection>,
            }

            /// The internal oneof alternatives carrying the authored tags.
            #[derive(Clone, Debug, PartialEq)]
            pub enum Selection {
                #(#selection_variants)*
            }

            impl ::phoxal::generated::prost::Message for Wire {
                fn encode_raw(
                    &self,
                    buf: &mut impl ::phoxal::generated::prost::bytes::BufMut,
                ) {
                    if let ::std::option::Option::Some(selection) = &self.selection {
                        ::phoxal::schema::PhoxalWire::phoxal_encode(selection, 0, buf);
                    }
                }

                fn merge_field(
                    &mut self,
                    tag: u32,
                    wire_type: ::phoxal::generated::prost::encoding::WireType,
                    buf: &mut impl ::phoxal::generated::prost::bytes::Buf,
                    ctx: ::phoxal::generated::prost::encoding::DecodeContext,
                ) -> ::std::result::Result<(), ::phoxal::generated::prost::DecodeError> {
                    match tag {
                        #(#wire_merge_arms)*
                        _ => ::phoxal::generated::prost::encoding::skip_field(
                            wire_type,
                            tag,
                            buf,
                            ctx,
                        ),
                    }
                }

                fn encoded_len(&self) -> usize {
                    self.selection.as_ref().map_or(
                        0,
                        |selection| ::phoxal::schema::PhoxalWire::phoxal_encoded_len(
                            selection,
                            0,
                        ),
                    )
                }

                fn clear(&mut self) {
                    *self = Self::default();
                }
            }

            impl ::phoxal::schema::PhoxalWire for Selection {
                const WIRE_TYPE: ::phoxal::generated::prost::encoding::WireType =
                    ::phoxal::generated::prost::encoding::WireType::LengthDelimited;
                const IS_ENUM: bool = false;
                const SKIP_DEFAULT: bool = false;

                fn phoxal_encode<B: ::phoxal::generated::prost::bytes::BufMut>(
                    &self,
                    _outer_tag: u32,
                    buf: &mut B,
                ) {
                    match self {
                        #(#selection_encode_arms)*
                    }
                }

                fn phoxal_write<B: ::phoxal::generated::prost::bytes::BufMut>(
                    &self,
                    buf: &mut B,
                ) {
                    match self {
                        #(#selection_write_arms)*
                    }
                }

                fn phoxal_value_len(&self) -> usize {
                    match self {
                        #(#selection_value_len_arms)*
                    }
                }

                fn phoxal_merge<B: ::phoxal::generated::prost::bytes::Buf>(
                    &mut self,
                    tag: u32,
                    wire_type: ::phoxal::generated::prost::encoding::WireType,
                    buf: &mut B,
                    ctx: ::phoxal::generated::prost::encoding::DecodeContext,
                ) -> ::std::result::Result<(), ::phoxal::generated::prost::DecodeError> {
                    match tag {
                        #(#selection_merge_arms)*
                        _ => {
                            return ::std::result::Result::Err(
                                ::phoxal::schema::unknown_oneof_tag(tag),
                            );
                        }
                    }
                    ::std::result::Result::Ok(())
                }

                fn phoxal_merge_fresh<B: ::phoxal::generated::prost::bytes::Buf>(
                    tag: u32,
                    wire_type: ::phoxal::generated::prost::encoding::WireType,
                    buf: &mut B,
                    ctx: ::phoxal::generated::prost::encoding::DecodeContext,
                ) -> ::std::result::Result<Self, ::phoxal::generated::prost::DecodeError> {
                    match tag {
                        #(#selection_fresh_arms)*
                        _ => ::std::result::Result::Err(
                            ::phoxal::schema::unknown_oneof_tag(tag),
                        ),
                    }
                }

                fn phoxal_encoded_len(&self, _outer_tag: u32) -> usize {
                    match self {
                        #(#selection_len_arms)*
                    }
                }
            }

            impl ::phoxal::contracts::ProstPayload for #enum_name {
                type Wire = Wire;

                fn to_wire(&self) -> Wire {
                    match self {
                        #(#to_wire_arms)*
                    }
                }

                fn try_from_wire(
                    wire: Wire,
                ) -> ::std::result::Result<Self, ::phoxal::generated::prost::DecodeError> {
                    match wire.selection {
                        #(#from_wire_arms)*
                        ::std::option::Option::None => ::std::result::Result::Err(
                            ::phoxal::schema::missing_variant_error(#wire_nested),
                        ),
                    }
                }
            }

            impl ::phoxal::schema::PhoxalWire for #enum_name {
                const WIRE_TYPE: ::phoxal::generated::prost::encoding::WireType =
                    ::phoxal::generated::prost::encoding::WireType::LengthDelimited;
                const IS_ENUM: bool = false;
                const SKIP_DEFAULT: bool = false;

                fn phoxal_write<B: ::phoxal::generated::prost::bytes::BufMut>(
                    &self,
                    buf: &mut B,
                ) {
                    let wire = <Self as ::phoxal::contracts::ProstPayload>::to_wire(self);
                    let len = <Wire as ::phoxal::generated::prost::Message>::encoded_len(&wire);
                    ::phoxal::generated::prost::encoding::encode_varint(len as u64, buf);
                    <Wire as ::phoxal::generated::prost::Message>::encode_raw(&wire, buf);
                }

                fn phoxal_value_len(&self) -> usize {
                    let wire = <Self as ::phoxal::contracts::ProstPayload>::to_wire(self);
                    let len = <Wire as ::phoxal::generated::prost::Message>::encoded_len(&wire);
                    ::phoxal::generated::prost::encoding::encoded_len_varint(len as u64) + len
                }

                fn phoxal_merge<B: ::phoxal::generated::prost::bytes::Buf>(
                    &mut self,
                    _tag: u32,
                    wire_type: ::phoxal::generated::prost::encoding::WireType,
                    buf: &mut B,
                    ctx: ::phoxal::generated::prost::encoding::DecodeContext,
                ) -> ::std::result::Result<(), ::phoxal::generated::prost::DecodeError> {
                    let mut wire = <Self as ::phoxal::contracts::ProstPayload>::to_wire(self);
                    ::phoxal::generated::prost::encoding::message::merge(
                        wire_type,
                        &mut wire,
                        buf,
                        ctx,
                    )?;
                    *self = <Self as ::phoxal::contracts::ProstPayload>::try_from_wire(wire)?;
                    ::std::result::Result::Ok(())
                }

                fn phoxal_merge_fresh<B: ::phoxal::generated::prost::bytes::Buf>(
                    tag: u32,
                    wire_type: ::phoxal::generated::prost::encoding::WireType,
                    buf: &mut B,
                    ctx: ::phoxal::generated::prost::encoding::DecodeContext,
                ) -> ::std::result::Result<Self, ::phoxal::generated::prost::DecodeError> {
                    let mut wire = <Wire as ::std::default::Default>::default();
                    ::phoxal::generated::prost::encoding::message::merge(
                        wire_type,
                        &mut wire,
                        buf,
                        ctx,
                    )?;
                    <Self as ::phoxal::contracts::ProstPayload>::try_from_wire(wire)
                }
            }

            #(#unit_payloads)*

            const MESSAGE_RECORD: ::phoxal::schema::SchemaRecord<'static> =
                ::phoxal::schema::SchemaRecord::Message(::phoxal::schema::MessageRecord {
                    package: #package_nested,
                    name: #schema_name,
                    fields: &[
                        ::phoxal::schema::FieldRecord {
                            number: #first_tag,
                            name: #envelope_lit,
                            ty: ::phoxal::schema::FieldType::Oneof,
                            label: ::phoxal::schema::Label::Singular,
                            oneof: ::std::option::Option::Some(#envelope_lit),
                        },
                    ],
                });

            const ONEOF_RECORD: ::phoxal::schema::SchemaRecord<'static> =
                ::phoxal::schema::SchemaRecord::Oneof(::phoxal::schema::OneofRecord {
                    package: #package_nested,
                    message: #schema_name,
                    field: #envelope_lit,
                    variants: &[#(#variant_records),*],
                });

            #[used]
            #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_schema"))]
            #[cfg_attr(target_os = "linux", unsafe(link_section = ".phoxal_schema"))]
            static MESSAGE_FRAME: [u8; ::phoxal::schema::encoded_len(&MESSAGE_RECORD)] = {
                let mut bytes = [0_u8; ::phoxal::schema::encoded_len(&MESSAGE_RECORD)];
                ::phoxal::schema::write_frame(&MESSAGE_RECORD, &mut bytes);
                bytes
            };

            #[used]
            #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_schema"))]
            #[cfg_attr(target_os = "linux", unsafe(link_section = ".phoxal_schema"))]
            static ONEOF_FRAME: [u8; ::phoxal::schema::encoded_len(&ONEOF_RECORD)] = {
                let mut bytes = [0_u8; ::phoxal::schema::encoded_len(&ONEOF_RECORD)];
                ::phoxal::schema::write_frame(&ONEOF_RECORD, &mut bytes);
                bytes
            };

            impl ::phoxal::schema::MessageSchema for #enum_name {
                const RECORD: ::phoxal::schema::SchemaRecord<'static> = MESSAGE_RECORD;
                const WIRE_NAME: &'static str = #wire_nested;

                fn retain_schema() -> usize {
                    let mut size = ::std::hint::black_box(&MESSAGE_FRAME).len();
                    size += ::std::hint::black_box(&ONEOF_FRAME).len();
                    #(
                        size += #variant_retention;
                    )*
                    size
                }
            }

            impl ::phoxal::schema::OneofSchema for #enum_name {
                const TAGS: &'static [u32] = #tag_list;
                const RECORD: ::phoxal::schema::SchemaRecord<'static> = ONEOF_RECORD;

                fn retain_schema() -> usize {
                    ::std::hint::black_box(&ONEOF_FRAME).len()
                }
            }
        }
    })
}

impl PayloadVariant {
    /// The wire-name expression of a unit variant's internal payload:
    /// the enum's own wire name joined with the variant's name.
    fn unit_wire_name(&self, wire_nested: &TokenStream) -> TokenStream {
        let variant_camel = LitStr::new(
            &self.ident.to_string().to_upper_camel_case(),
            self.ident.span(),
        );
        quote!(::phoxal::phoxal_joined_wire!(#wire_nested, #variant_camel))
    }
}

fn expand_enum(options: MessageOptions, item: ItemEnum) -> syn::Result<TokenStream> {
    if !item.generics.params.is_empty() {
        return Err(syn::Error::new_spanned(
            &item.generics,
            "#[phoxal::message] does not support generic enumerations",
        ));
    }
    // Variants carrying explicit phoxal tags declare a payload enum: an
    // ordinary Rust enum that owns its Protobuf message envelope.
    let variant_tags: Vec<Option<u32>> = item
        .variants
        .iter()
        .map(|variant| variant_meta(variant).ok().and_then(|meta| meta.tag))
        .collect();
    if variant_tags.iter().any(Option::is_some) {
        if item
            .variants
            .iter()
            .any(|variant| variant.discriminant.is_some())
        {
            return Err(syn::Error::new_spanned(
                &item,
                "a tag-carrying payload enum does not mix numeric discriminants into variants",
            ));
        }
        if variant_tags.iter().any(Option::is_none) {
            return Err(syn::Error::new_spanned(
                &item,
                "every variant of a payload enum declares its tag, e.g. #[phoxal(tag = 1)]",
            ));
        }
        return expand_payload_enum(options, item);
    }
    let enum_name = item.ident.clone();
    let schema_name = options
        .schema_name
        .clone()
        .unwrap_or_else(|| LitStr::new(&enum_name.to_string(), enum_name.span()));
    let module = format_ident!("phoxal_schema_{}", enum_name.to_string().to_snake_case());
    let shouting_name = schema_name.value().to_shouty_snake_case();
    let PackageForms {
        package_nested,
        wire_nested,
        declaration: package_declaration,
        ..
    } = options.package.forms(&schema_name, &shouting_name);

    let mut rebuilt_variants = Vec::new();
    let mut values = Vec::new();
    for variant in &item.variants {
        if !matches!(variant.fields, Fields::Unit) {
            return Err(syn::Error::new_spanned(
                variant,
                "#[phoxal::message] enumerations declare plain discriminants",
            ));
        }
        let (_, discriminant) = variant.discriminant.as_ref().ok_or_else(|| {
            syn::Error::new_spanned(
                variant,
                "every enumeration variant declares its wire number explicitly",
            )
        })?;
        let Expr::Lit(ExprLit {
            lit: Lit::Int(number),
            ..
        }) = discriminant
        else {
            return Err(syn::Error::new_spanned(
                discriminant,
                "enumeration wire numbers are integer literals",
            ));
        };
        let number_text = number.base10_parse::<i32>().map_err(|_| {
            syn::Error::new_spanned(number, "enumeration wire numbers are i32 values")
        })?;
        let value_name = variant_value_name(&variant.attrs, &shouting_name, &variant.ident);
        let attrs: Vec<_> = variant
            .attrs
            .iter()
            .filter(|attr| !attr.path().is_ident("phoxal"))
            .cloned()
            .collect();
        let ident = &variant.ident;
        rebuilt_variants.push(quote! {
            #(#attrs)*
            #ident = #number,
        });
        values.push(quote!(
            ::phoxal::schema::EnumValue {
                number: #number_text,
                name: #value_name,
            }
        ));
    }

    let vis = &item.vis;
    let enum_attrs: Vec<_> = item
        .attrs
        .iter()
        .filter(|attr| !attr.path().is_ident("phoxal"))
        .cloned()
        .collect();

    Ok(quote! {
        #package_declaration
        #(#enum_attrs)*
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord, ::phoxal::generated::prost::Enumeration)]
        #[prost(prost_path = "::phoxal::generated::prost")]
        #[repr(i32)]
        #vis enum #enum_name {
            #(#rebuilt_variants)*
        }

        impl ::phoxal::schema::PhoxalWire for #enum_name {
            const WIRE_TYPE: ::phoxal::generated::prost::encoding::WireType =
                ::phoxal::generated::prost::encoding::WireType::Varint;
            const IS_ENUM: bool = true;
            const SKIP_DEFAULT: bool = true;

            fn phoxal_write<B: ::phoxal::generated::prost::bytes::BufMut>(&self, buf: &mut B) {
                let raw: i32 = ::std::convert::From::from(*self);
                ::phoxal::generated::prost::encoding::encode_varint(raw as i64 as u64, buf);
            }

            fn phoxal_value_len(&self) -> usize {
                let raw: i32 = ::std::convert::From::from(*self);
                ::phoxal::generated::prost::encoding::encoded_len_varint(raw as i64 as u64)
            }

            fn phoxal_merge<B: ::phoxal::generated::prost::bytes::Buf>(
                &mut self,
                _tag: u32,
                wire_type: ::phoxal::generated::prost::encoding::WireType,
                buf: &mut B,
                ctx: ::phoxal::generated::prost::encoding::DecodeContext,
            ) -> ::std::result::Result<(), ::phoxal::generated::prost::DecodeError> {
                let mut raw: i32 = ::std::convert::From::from(*self);
                ::phoxal::generated::prost::encoding::int32::merge(
                    wire_type,
                    &mut raw,
                    buf,
                    ctx,
                )?;
                *self = ::std::convert::TryFrom::try_from(raw)
                    .map_err(::phoxal::schema::unknown_enum_value_error)?;
                ::std::result::Result::Ok(())
            }

            fn phoxal_merge_fresh<B: ::phoxal::generated::prost::bytes::Buf>(
                tag: u32,
                wire_type: ::phoxal::generated::prost::encoding::WireType,
                buf: &mut B,
                ctx: ::phoxal::generated::prost::encoding::DecodeContext,
            ) -> ::std::result::Result<Self, ::phoxal::generated::prost::DecodeError> {
                ::phoxal::schema::merge_fresh_default(tag, wire_type, buf, ctx)
            }
        }

        mod #module {
            use super::#enum_name;

            const RECORD: ::phoxal::schema::SchemaRecord<'static> =
                ::phoxal::schema::SchemaRecord::Enum(::phoxal::schema::EnumRecord {
                    package: #package_nested,
                    name: #schema_name,
                    values: &[#(#values),*],
                });

            #[used]
            #[cfg_attr(target_os = "macos", unsafe(link_section = "__DATA,__phoxal_schema"))]
            #[cfg_attr(target_os = "linux", unsafe(link_section = ".phoxal_schema"))]
            static FRAME: [u8; ::phoxal::schema::encoded_len(&RECORD)] = {
                let mut bytes = [0_u8; ::phoxal::schema::encoded_len(&RECORD)];
                ::phoxal::schema::write_frame(&RECORD, &mut bytes);
                bytes
            };

            impl ::phoxal::schema::MessageSchema for #enum_name {
                const RECORD: ::phoxal::schema::SchemaRecord<'static> = RECORD;
                const WIRE_NAME: &'static str = #wire_nested;

                fn retain_schema() -> usize {
                    ::std::hint::black_box(&FRAME).len()
                }
            }
        }
    })
}

/// Returns the Protobuf value name of one enumeration variant.
///
/// The default follows Prost's convention of prefixing the variant with the
/// enumeration's own name; `#[phoxal(name = "...")]` overrides it.
fn variant_value_name(attrs: &[syn::Attribute], shouting_name: &str, ident: &Ident) -> LitStr {
    for attr in attrs.iter().filter(|attr| attr.path().is_ident("phoxal")) {
        if let Ok(name) = attr.parse_args::<LitStr>() {
            return name;
        }
    }
    LitStr::new(
        &format!(
            "{}_{}",
            shouting_name,
            ident.to_string().to_shouty_snake_case()
        ),
        ident.span(),
    )
}

/// Expands `#[phoxal::messages(package = "...")] mod ... { ... }`.
///
/// Declares the Protobuf package once for every message, enumeration, and
/// payload enum authored directly inside the inline module; an item that
/// declares its own package keeps that spelling. Inline modules are the
/// supported surface: an attribute on an external module declaration is
/// rejected rather than silently reading another file.
pub(crate) fn expand_messages(attr: TokenStream, item: TokenStream) -> syn::Result<TokenStream> {
    let mut package: Option<LitStr> = None;
    syn::meta::parser(|meta| {
        if meta.path.is_ident("package") {
            package = Some(meta.value()?.parse()?);
            Ok(())
        } else {
            Err(meta.error("unsupported #[phoxal::messages] option; expected package"))
        }
    })
    .parse2(attr)?;
    let package = package.ok_or_else(|| {
        syn::Error::new(Span::call_site(), "#[phoxal::messages] requires package")
    })?;
    let mut module: syn::ItemMod = syn::parse2(item)?;
    let Some((_, items)) = module.content.as_mut() else {
        return Err(syn::Error::new_spanned(
            &module,
            "#[phoxal::messages] declares an inline module; an external module \
             declaration cannot carry its items' package",
        ));
    };
    for item in items.iter_mut() {
        match item {
            syn::Item::Struct(value) => {
                let kind = if value
                    .attrs
                    .iter()
                    .any(|attr| is_phoxal_path(attr.path(), "endpoints"))
                {
                    "endpoints"
                } else {
                    "message"
                };
                ensure_package(&mut value.attrs, &package, kind)?;
            }
            syn::Item::Enum(value) => ensure_package(&mut value.attrs, &package, "message")?,
            _ => continue,
        }
    }
    Ok(quote!(#module))
}

/// Ensures one item carries the phoxal attribute `name` declaring the
/// block's package: an existing attribute without a package option gains
/// one, and an unannotated item is annotated outright. The item's complete
/// authored argument list is parsed and preserved, so every option the
/// standalone attribute accepts stays available inside the module.
fn ensure_package(
    attrs: &mut Vec<syn::Attribute>,
    package: &LitStr,
    name: &str,
) -> syn::Result<()> {
    let macro_name = Ident::new(name, Span::call_site());
    if let Some(index) = attrs
        .iter()
        .position(|attr| is_phoxal_path(attr.path(), name))
    {
        let mut options = attribute_options(&attrs[index])?;
        if options
            .iter()
            .any(|option| matches!(option, syn::Meta::NameValue(named) if named.path.is_ident("package")))
        {
            return Ok(());
        }
        options.push(syn::parse_quote!(package = #package));
        let tokens = quote!(#(#options),*);
        attrs[index] = syn::parse_quote!(#[phoxal::#macro_name(#tokens)]);
        return Ok(());
    }
    attrs.push(syn::parse_quote!(#[phoxal::#macro_name(package = #package)]));
    Ok(())
}

/// Parses one phoxal attribute's complete argument list structurally:
/// every option with its value, in authored order. A bare or empty
/// attribute carries no options.
fn attribute_options(attr: &syn::Attribute) -> syn::Result<Vec<syn::Meta>> {
    let syn::Meta::List(list) = &attr.meta else {
        return Ok(Vec::new());
    };
    let options = syn::punctuated::Punctuated::<syn::Meta, syn::Token![,]>::parse_terminated
        .parse2(list.tokens.clone())?;
    Ok(options.into_iter().collect())
}

/// Whether a path names `phoxal::<name>` or the bare `<name>` form the
/// re-exported macro allows.
fn is_phoxal_path(path: &syn::Path, name: &str) -> bool {
    let segments: Vec<_> = path.segments.iter().map(|s| s.ident.to_string()).collect();
    match segments.as_slice() {
        [one] => one == name,
        [root, one] => root == "phoxal" && one == name,
        _ => false,
    }
}
