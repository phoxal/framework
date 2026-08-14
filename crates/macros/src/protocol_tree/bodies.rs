//! The descriptor side of a generated family: the family-local `Api` marker,
//! payload-only domain facades, and the separate endpoint descriptor tree that
//! binds ordinary payloads to wire keys.
//!
//! The parallel topic-builder tree is emitted by [`super::builders`] and
//! spliced into the same tree module here.

use super::model::{Descriptor, MaterializedTree, Node};
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};

impl MaterializedTree {
    /// Emit this tree's module: an API marker, domain facades, endpoint
    /// descriptors, and side-branded topic builders.
    pub(super) fn expand(&self) -> TokenStream {
        let mod_name = &self.module;
        let mut node_mods = TokenStream::new();
        for node in &self.nodes {
            node_mods.extend(node.expand_module(&self.module, &self.source));
        }

        let topic_mod = self.expand_topic_module();
        let endpoint_mod = self.expand_endpoint_module();
        let module_doc = &self.doc;
        let id = &self.id;

        quote! {
            #[doc = #module_doc]
            pub mod #mod_name {
                /// Zero-variant marker identifying this contract family.
                #[derive(Clone, Copy, Debug)]
                pub enum Api {}
                impl ::phoxal_bus::ApiFamily for Api {
                    const ID: &'static str = #id;
                }

                // Every generated domain module forwards this local anchor one
                // hop, keeping endpoint `Api` paths independent of tree depth.
                #[doc(hidden)]
                pub use self::Api as __ProtocolMarker;

                #node_mods

                /// Generated endpoint descriptors, separate from payload
                /// domains so one payload can serve several endpoints.
                pub mod endpoint {
                    #[doc(hidden)]
                    pub use super::Api as __ProtocolMarker;

                    #endpoint_mod
                }

                #topic_mod
            }
        }
    }
}

impl MaterializedTree {
    fn expand_endpoint_module(&self) -> TokenStream {
        self.nodes
            .iter()
            .map(|node| node.expand_endpoint_module(&self.id, "", ""))
            .collect()
    }
}

impl Node {
    /// Emit one family-local payload facade.
    fn expand_module(&self, tree_module: &syn::Ident, source: &syn::Path) -> TokenStream {
        let name = &self.name;
        let mut external_aliases = TokenStream::new();
        let mut external_parents = std::collections::BTreeMap::<String, syn::Path>::new();
        let mut semantic_aliases = std::collections::BTreeMap::<String, syn::Path>::new();

        for topic in &self.topics {
            match &topic.descriptor {
                Descriptor::State(body)
                | Descriptor::Sample(body)
                | Descriptor::Event(body)
                | Descriptor::Setpoint(body)
                | Descriptor::Stream { body, .. } => {
                    let payload = &body.path;
                    collect_external_parent(&mut external_parents, payload);
                    register_semantic_alias(&mut semantic_aliases, payload, tree_module, source);
                }
                Descriptor::Query { request, response } => {
                    let request = &request.path;
                    let response = &response.path;
                    collect_external_parent(&mut external_parents, request);
                    collect_external_parent(&mut external_parents, response);
                    register_semantic_alias(&mut semantic_aliases, request, tree_module, source);
                    register_semantic_alias(&mut semantic_aliases, response, tree_module, source);
                }
            }
        }

        let authoritative_parent = external_parents
            .values()
            .find(|path| is_current_family_path(path, tree_module, source))
            .or_else(|| external_parents.values().next());
        let authoritative_key = authoritative_parent.map(ToTokens::to_token_stream);
        for body_path in semantic_aliases.values() {
            let mut parent = body_path.clone();
            parent.segments.pop();
            parent.segments.pop_punct();
            if authoritative_key.as_ref().is_some_and(|authoritative| {
                parent.to_token_stream().to_string() == authoritative.to_string()
            }) {
                continue;
            }
            let Some(last) = body_path.segments.last() else {
                continue;
            };
            let alias = syn::Ident::new(&last.ident.to_string(), last.ident.span());
            external_aliases
                .extend(quote! { #[allow(unused_imports)] pub use #body_path as #alias; });
        }
        let external_parent_import = authoritative_parent
            .map(|parent| quote! { #[allow(unused_imports)] pub use #parent::*; });
        let child_mods = self
            .children
            .iter()
            .map(|child| child.expand_module(tree_module, source))
            .collect::<TokenStream>();
        quote! {
            pub mod #name {
                /// Family-local payload facade for this endpoint node.

                #external_parent_import
                #external_aliases
                #child_mods
            }
        }
    }

    fn expand_endpoint_module(
        &self,
        tree_id: &str,
        family_prefix: &str,
        key_prefix: &str,
    ) -> TokenStream {
        let name = &self.name;
        let family_path = self.family_path(family_prefix);
        let node_key_prefix = self.key_prefix(key_prefix);
        let mut descriptors = TokenStream::new();
        for topic in &self.topics {
            let key = format!("{tree_id}/{}", topic.leaf.key(&node_key_prefix));
            let endpoint = topic.endpoint_ident();
            let endpoint_name = format!("{tree_id}::{family_path}::{endpoint}");
            let endpoint_contract = format!("{family_path}::{endpoint}");
            let endpoint_kind = topic.endpoint_kind();
            let semantic_doc = topic.descriptor.rustdoc();
            match &topic.descriptor {
                Descriptor::State(body)
                | Descriptor::Sample(body)
                | Descriptor::Event(body)
                | Descriptor::Setpoint(body)
                | Descriptor::Stream { body, .. } => {
                    let payload = &body.path;
                    let delivery_marker = topic.descriptor.delivery_marker_trait();
                    let temporal_marker = topic
                        .descriptor
                        .semantic_marker_trait()
                        .map(|marker| quote! { impl #marker for #endpoint {} })
                        .unwrap_or_default();
                    descriptors.extend(quote! {
                        #[doc = #semantic_doc]
                        #[derive(Clone, Copy, Debug)]
                        pub struct #endpoint;
                        impl ::phoxal_bus::EndpointDescriptor for #endpoint {
                            type Api = self::__ProtocolMarker;
                            type Payload = #payload;
                            const NAME: &'static str = #endpoint_name;
                            const FAMILY: &'static str = #tree_id;
                            const CONTRACT: &'static str = #endpoint_contract;
                            const TOPIC: &'static str = #key;
                            const KIND: ::phoxal_bus::EndpointKind = #endpoint_kind;
                        }
                        impl #delivery_marker for #endpoint {}
                        #temporal_marker
                    });
                }
                Descriptor::Query { request, response } => {
                    let request = &request.path;
                    let response = &response.path;
                    descriptors.extend(quote! {
                        #[doc = #semantic_doc]
                        #[derive(Clone, Copy, Debug)]
                        pub struct #endpoint;
                        impl ::phoxal_bus::EndpointDescriptor for #endpoint {
                            type Api = self::__ProtocolMarker;
                            type Payload = #request;
                            const NAME: &'static str = #endpoint_name;
                            const FAMILY: &'static str = #tree_id;
                            const CONTRACT: &'static str = #endpoint_contract;
                            const TOPIC: &'static str = #key;
                            const KIND: ::phoxal_bus::EndpointKind = #endpoint_kind;
                        }
                        impl ::phoxal_bus::QueryEndpointDescriptor for #endpoint {
                            type Request = #request;
                            type Response = #response;
                        }
                    });
                }
            }
        }
        let children = self
            .children
            .iter()
            .map(|child| child.expand_endpoint_module(tree_id, &family_path, &node_key_prefix))
            .collect::<TokenStream>();
        quote! {
            pub mod #name {
                #[doc(hidden)]
                pub use super::__ProtocolMarker;

                #descriptors
                #children
            }
        }
    }
}

/// Record one authored domain module used by an endpoint at this logical path.
fn collect_external_parent(
    parents: &mut std::collections::BTreeMap<String, syn::Path>,
    path: &syn::Path,
) {
    if path.segments.len() < 2 {
        return;
    }
    let mut parent = path.clone();
    parent.segments.pop();
    parent.segments.pop_punct();
    parents
        .entry(parent.to_token_stream().to_string())
        .or_insert(parent);
}

fn register_semantic_alias(
    aliases: &mut std::collections::BTreeMap<String, syn::Path>,
    path: &syn::Path,
    tree_module: &syn::Ident,
    source: &syn::Path,
) {
    let Some(last) = path.segments.last() else {
        return;
    };
    let name = last.ident.to_string();
    let prefer = is_current_family_path(path, tree_module, source);
    if aliases
        .get(&name)
        .is_none_or(|existing| prefer && !is_current_family_path(existing, tree_module, source))
    {
        aliases.insert(name, path.clone());
    }
}

fn is_current_family_path(path: &syn::Path, tree_module: &syn::Ident, source: &syn::Path) -> bool {
    if path.leading_colon.is_some() != source.leading_colon.is_some()
        || path.segments.len() <= source.segments.len()
        || !path
            .segments
            .iter()
            .zip(&source.segments)
            .all(|(actual, expected)| {
                actual.ident == expected.ident
                    && actual.arguments.to_token_stream().to_string()
                        == expected.arguments.to_token_stream().to_string()
            })
    {
        return false;
    }
    path.segments[source.segments.len()].ident == *tree_module
}
