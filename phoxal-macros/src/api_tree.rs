//! `phoxal_api_tree!` — the single API layer (D60/D61).
//!
//! Grammar (first slice — `extends` and parameterized/dynamic topics are
//! deferred to a later slice):
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
//! }
//! ```
//!
//! Each `version` becomes a `pub mod y2026_N` carrying a marker `enum Api {}`
//! (`ApiVersion`), the version-local body/helper types (plain serde types, no
//! `{"v":…}` wrapper — D62), `ContractBody` impls binding each on-bus body to
//! that version, and an api-local `topic` builder module.

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
    families: Vec<Family>,
}

struct Family {
    name: Ident,
    types: Vec<TypeDef>,
    topics: Vec<TopicDef>,
}

enum TypeDef {
    Struct(ItemStruct),
    Enum(ItemEnum),
}

struct TopicDef {
    leaf: Ident,
    kind: TopicKind,
}

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
        if input.peek(kw::extends) {
            let kw = input.parse::<kw::extends>()?;
            return Err(syn::Error::new(
                kw.span,
                "`extends` (API inheritance) is deferred to a later slice; the first cut is \
                 single-version (y2026_1). See implementation-order.md Phase 1.",
            ));
        }
        let body;
        syn::braced!(body in input);
        let mut families = Vec::new();
        while !body.is_empty() {
            families.push(body.parse()?);
        }
        Ok(Version { name, families })
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
        for version in &self.versions {
            out.extend(version.expand()?);
        }
        Ok(out)
    }
}

impl Version {
    fn expand(&self) -> syn::Result<TokenStream> {
        let phoxal = phoxal();
        let mod_name = &self.name;
        let id = self.name.to_string();

        let mut family_mods = TokenStream::new();
        for family in &self.families {
            family_mods.extend(family.expand_module()?);
        }

        let topic_mod = self.expand_topic_module()?;

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

    fn expand_topic_module(&self) -> syn::Result<TokenStream> {
        let phoxal = phoxal();
        let mut root_methods = TokenStream::new();
        let mut builder_mods = TokenStream::new();

        for family in &self.families {
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
