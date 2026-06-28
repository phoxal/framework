//! `phoxal_api_tree!` — the single API layer (D60/D61).
//!
//! Grammar (`extends` API inheritance is supported; parameterized/dynamic topics
//! are a later slice):
//!
//! ```text
//! phoxal_api_tree! {
//!     version y2026_1 {
//!         drive {
//!             enum  StopReason { NoTarget, Estop }
//!             enum  ActuatorAuthority { Active, Stopped }
//!             struct Target { linear_x_mps: f32, angular_z_radps: f32 }
//!             struct State  { target: Target, authority: ActuatorAuthority }
//!             topic target: pubsub Target;
//!             topic state:  pubsub State;
//!         }
//!         map {
//!             struct SubmapRequest  { region: u32 }
//!             struct SubmapResponse { cells: Vec<u8> }
//!             topic submap: query SubmapRequest => SubmapResponse;
//!         }
//!     }
//!     version y2026_2 extends y2026_1 {
//!         // inherits every y2026_1 family/type; overrides drive::Target and adds
//!         // a new `battery` family — inherited types are re-emitted fresh.
//!         drive { struct Target { linear_x_mps: f32, angular_z_radps: f32, curvature: Option<f32> }
//!                 topic target: pubsub Target; }
//!         battery { struct State { soc: f32 } topic state: pubsub State; }
//!     }
//! }
//! ```
//!
//! Each `version` becomes a `pub mod y2026_N` carrying a marker `enum Api {}`
//! (`ApiVersion`), the version-local body/helper types (plain serde types, no
//! `{"v":…}` wrapper — D62), `ContractBody` impls binding each on-bus body to
//! that version, and an api-local `topic` builder module. A `version … extends P`
//! inherits `P`'s effective families: a child family merges into the parent
//! family of the same name (child types/topics override by name), a new child
//! family is added, and every inherited type is re-emitted *fresh* under the
//! child version — same `FAMILY`/`TOPIC`, a different `Api` (D61).
//!
//! Wire identity is **per-type, by transitive shape**: an inherited type re-emits
//! the parent's definition verbatim, so a type that does **not** transitively
//! reference an overridden type is byte-identical to its parent (e.g. a flat
//! `localize::State`). A type that *contains* an overridden type resolves that
//! name to the child's overridden version — e.g. if the child overrides
//! `drive::Target`, the inherited `drive::State { target: Target, … }` embeds the
//! **new** `Target` and is therefore *not* identical to the parent's `State`.
//! That is correct versioning: a contract changes with its dependencies, and the
//! whole change is coherent because one graph runs one API version (D59).

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, ItemEnum, ItemStruct, Token};

use crate::util::{body_derives, phoxal};

mod kw {
    syn::custom_keyword!(version);
    syn::custom_keyword!(extends);
    syn::custom_keyword!(topic);
    syn::custom_keyword!(pubsub);
    syn::custom_keyword!(query);
}

pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let tree: ApiTree = syn::parse2(input)?;
    tree.expand()
}

struct ApiTree {
    versions: Vec<Version>,
}

struct Version {
    name: Ident,
    extends: Option<Ident>,
    families: Vec<Family>,
}

#[derive(Clone)]
struct Family {
    name: Ident,
    types: Vec<TypeDef>,
    topics: Vec<TopicDef>,
}

#[derive(Clone)]
enum TypeDef {
    Struct(ItemStruct),
    Enum(ItemEnum),
}

impl TypeDef {
    /// The declared name of the type (for inheritance overlay by name).
    fn ident(&self) -> &Ident {
        match self {
            TypeDef::Struct(item) => &item.ident,
            TypeDef::Enum(item) => &item.ident,
        }
    }
}

#[derive(Clone)]
struct TopicDef {
    leaf: Ident,
    kind: TopicKind,
}

#[derive(Clone)]
enum TopicKind {
    PubSub(Ident),
    Query { request: Ident, response: Ident },
}

impl Parse for ApiTree {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut versions = Vec::new();
        while !input.is_empty() {
            versions.push(input.parse()?);
        }
        if versions.is_empty() {
            return Err(input.error("phoxal_api_tree! requires at least one `version` block"));
        }
        Ok(ApiTree { versions })
    }
}

impl Parse for Version {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::version>()?;
        let name: Ident = input.parse()?;
        let extends = if input.peek(kw::extends) {
            input.parse::<kw::extends>()?;
            Some(input.parse::<Ident>()?)
        } else {
            None
        };
        let body;
        syn::braced!(body in input);
        let mut families = Vec::new();
        while !body.is_empty() {
            families.push(body.parse()?);
        }
        Ok(Version {
            name,
            extends,
            families,
        })
    }
}

impl Parse for Family {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        let body;
        syn::braced!(body in input);
        let mut types = Vec::new();
        let mut topics = Vec::new();
        while !body.is_empty() {
            // Leading doc-comments / attributes apply to the next item; `topic`
            // declarations take none.
            let attrs = body.call(syn::Attribute::parse_outer)?;
            if body.peek(kw::topic) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a `topic` declaration",
                    ));
                }
                topics.push(body.parse()?);
            } else if body.peek(Token![struct]) {
                let mut item: ItemStruct = body.parse()?;
                item.attrs = attrs;
                item.vis = syn::Visibility::Public(syn::token::Pub::default());
                types.push(TypeDef::Struct(item));
            } else if body.peek(Token![enum]) {
                let mut item: ItemEnum = body.parse()?;
                item.attrs = attrs;
                item.vis = syn::Visibility::Public(syn::token::Pub::default());
                types.push(TypeDef::Enum(item));
            } else {
                return Err(body
                    .error("expected `struct`, `enum`, or `topic …;` inside an API family block"));
            }
        }
        Ok(Family {
            name,
            types,
            topics,
        })
    }
}

impl Parse for TopicDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::topic>()?;
        let leaf: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        let kind = if input.peek(kw::pubsub) {
            input.parse::<kw::pubsub>()?;
            let body: Ident = input.parse()?;
            TopicKind::PubSub(body)
        } else if input.peek(kw::query) {
            input.parse::<kw::query>()?;
            let request: Ident = input.parse()?;
            input.parse::<Token![=>]>()?;
            let response: Ident = input.parse()?;
            TopicKind::Query { request, response }
        } else {
            return Err(input.error("expected `pubsub <Type>` or `query <Req> => <Resp>`"));
        };
        input.parse::<Token![;]>()?;
        Ok(TopicDef { leaf, kind })
    }
}

impl ApiTree {
    fn expand(&self) -> syn::Result<TokenStream> {
        let mut out = TokenStream::new();
        // Resolve each version's effective families (inheriting from `extends`)
        // and generate from the resolved set. Inherited types are re-emitted
        // *fresh* under the child version — same `FAMILY`/`TOPIC`, a different
        // `Api` marker — so unchanged contracts are wire-identical by construction
        // while the compile-time `Api` bound stays sound (D61).
        let mut resolved: std::collections::HashMap<String, Vec<Family>> =
            std::collections::HashMap::new();

        for version in &self.versions {
            let effective = match &version.extends {
                None => version.families.clone(),
                Some(parent) => {
                    let base = resolved.get(&parent.to_string()).ok_or_else(|| {
                        syn::Error::new_spanned(
                            parent,
                            format!(
                                "`extends {parent}`: unknown API version (it must be declared \
                                 earlier in the same phoxal_api_tree! invocation)"
                            ),
                        )
                    })?;
                    overlay_families(base, &version.families)
                }
            };
            resolved.insert(version.name.to_string(), effective.clone());
            out.extend(expand_version(&version.name, &effective)?);
        }
        Ok(out)
    }
}

/// Overlay a child version's declared families onto the parent's effective set:
/// a child family merges into the parent family of the same name (child types
/// override parent types by name; child topics override by leaf), and a wholly
/// new child family is appended (D61).
fn overlay_families(base: &[Family], child: &[Family]) -> Vec<Family> {
    let mut result: Vec<Family> = base.to_vec();
    for cf in child {
        if let Some(bf) = result.iter_mut().find(|f| f.name == cf.name) {
            for ct in &cf.types {
                if let Some(slot) = bf.types.iter_mut().find(|t| t.ident() == ct.ident()) {
                    *slot = ct.clone();
                } else {
                    bf.types.push(ct.clone());
                }
            }
            for ctp in &cf.topics {
                if let Some(slot) = bf.topics.iter_mut().find(|t| t.leaf == ctp.leaf) {
                    *slot = ctp.clone();
                } else {
                    bf.topics.push(ctp.clone());
                }
            }
        } else {
            result.push(cf.clone());
        }
    }
    result
}

fn expand_version(name: &Ident, families: &[Family]) -> syn::Result<TokenStream> {
    let phoxal = phoxal();
    let mod_name = name;
    let id = name.to_string();

    let mut family_mods = TokenStream::new();
    for family in families {
        family_mods.extend(family.expand_module()?);
    }

    let topic_mod = expand_topic_module(families)?;

    Ok(quote! {
        pub mod #mod_name {
            //! Dated API version `#id` — version-local wire bodies + topics.

            /// Zero-variant marker identifying this API version (D60).
            #[derive(Clone, Copy, Debug)]
            pub enum Api {}
            impl #phoxal::api::ApiVersion for Api {
                const ID: &'static str = #id;
            }

            #family_mods

            #topic_mod
        }
    })
}

fn expand_topic_module(families: &[Family]) -> syn::Result<TokenStream> {
    let phoxal = phoxal();
    let mut root_methods = TokenStream::new();
    let mut builder_mods = TokenStream::new();

    for family in families {
        let fam = &family.name;
        let fam_str = fam.to_string();

        root_methods.extend(quote! {
            /// Enter the `#fam_str` family topic builder.
            pub fn #fam(self) -> #fam::Builder { #fam::Builder }
        });

        let mut leaf_methods = TokenStream::new();
        for topic in &family.topics {
            let leaf = &topic.leaf;
            let key = format!("{}/{}", fam_str, leaf);
            match &topic.kind {
                TopicKind::PubSub(body) => {
                    leaf_methods.extend(quote! {
                        #[doc = #key]
                        pub fn #leaf(self)
                            -> #phoxal::bus::Topic<#phoxal::bus::PubSub<super::super::#fam::#body>>
                        {
                            #phoxal::bus::Topic::new_static(#key)
                        }
                    });
                }
                TopicKind::Query { request, response } => {
                    leaf_methods.extend(quote! {
                        #[doc = #key]
                        pub fn #leaf(self)
                            -> #phoxal::bus::Topic<
                                #phoxal::bus::Query<
                                    super::super::#fam::#request,
                                    super::super::#fam::#response,
                                >,
                            >
                        {
                            #phoxal::bus::Topic::new_static(#key)
                        }
                    });
                }
            }
        }

        builder_mods.extend(quote! {
            pub mod #fam {
                /// Topic builder for the `#fam_str` family.
                pub struct Builder;
                impl Builder {
                    #leaf_methods
                }
            }
        });
    }

    Ok(quote! {
        /// Api-local topic builders (D61). `topic::new()` is the entrypoint;
        /// every leaf binds the topic's family/kind to a version-local body.
        pub mod topic {
            /// Begin a topic path for this API version.
            pub fn new() -> Root {
                Root
            }

            /// Root of the topic builder chain.
            pub struct Root;
            impl Root {
                #root_methods
            }

            #builder_mods
        }
    })
}

impl Family {
    fn expand_module(&self) -> syn::Result<TokenStream> {
        let phoxal = phoxal();
        let fam = &self.name;
        let fam_str = fam.to_string();
        let derives = body_derives();

        let mut types = TokenStream::new();
        for ty in &self.types {
            match ty {
                TypeDef::Struct(item) => {
                    let item = with_pub_fields_struct(item.clone());
                    types.extend(quote! { #derives #item });
                }
                TypeDef::Enum(item) => {
                    types.extend(quote! { #derives #item });
                }
            }
        }

        let mut impls = TokenStream::new();
        for topic in &self.topics {
            let leaf = &topic.leaf;
            let key = format!("{}/{}", fam_str, leaf);
            match &topic.kind {
                TopicKind::PubSub(body) => {
                    let family_const = format!("{}::{}", fam_str, body);
                    impls.extend(quote! {
                        impl #phoxal::api::ContractBody for #body {
                            type Api = super::Api;
                            const FAMILY: &'static str = #family_const;
                            const TOPIC: &'static str = #key;
                        }
                    });
                }
                TopicKind::Query { request, response } => {
                    let req_family = format!("{}::{}", fam_str, request);
                    let resp_family = format!("{}::{}", fam_str, response);
                    impls.extend(quote! {
                        impl #phoxal::api::ContractBody for #request {
                            type Api = super::Api;
                            const FAMILY: &'static str = #req_family;
                            const TOPIC: &'static str = #key;
                        }
                        impl #phoxal::api::ContractBody for #response {
                            type Api = super::Api;
                            const FAMILY: &'static str = #resp_family;
                            const TOPIC: &'static str = #key;
                        }
                    });
                }
            }
        }

        Ok(quote! {
            pub mod #fam {
                //! Version-local bodies for the `#fam_str` family.
                #![allow(unused_imports)]
                use super::Api;

                #types
                #impls
            }
        })
    }
}

/// Force every named field of a macro-declared body struct to `pub` so runtime
/// code in other modules can construct and read the wire body directly.
fn with_pub_fields_struct(mut item: ItemStruct) -> ItemStruct {
    if let syn::Fields::Named(named) = &mut item.fields {
        for field in &mut named.named {
            field.vis = syn::Visibility::Public(syn::token::Pub::default());
        }
    }
    item
}
