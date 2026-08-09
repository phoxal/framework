//! The descriptor side of a generated tree: the tree-local `Api` marker,
//! domain facades, and endpoint descriptors that bind ordinary payloads to
//! wire keys.
//!
//! The parallel topic-builder tree is emitted by [`super::builders`] and
//! spliced into the same tree module here.

use super::model::{MaterializedTree, Node, TopicKind};
use proc_macro2::TokenStream;
use quote::{ToTokens, quote};

impl MaterializedTree {
    /// Emit this tree's module: an API marker, domain facades, endpoint
    /// descriptors, and side-branded topic builders.
    pub(super) fn expand(&self) -> TokenStream {
        let mod_name = &self.module;
        let mut node_mods = TokenStream::new();
        for node in &self.nodes {
            node_mods.extend(node.expand_module(&self.id, "", ""));
        }

        let topic_mod = self.expand_topic_module();
        let endpoint_mod = self.expand_endpoint_module();
        let module_doc = &self.doc;
        let id = &self.id;

        quote! {
            #[doc = #module_doc]
            pub mod #mod_name {
                /// Zero-variant marker identifying this tree: an API revision
                /// in `version` mode or a protocol in `protocol` mode.
                #[derive(Clone, Copy, Debug)]
                pub enum Api {}
                impl ::phoxal_bus::ApiVersion for Api {
                    const ID: &'static str = #id;
                }

                // Every generated domain module forwards this local anchor one
                // hop, keeping endpoint `Api` paths independent of tree depth.
                #[doc(hidden)]
                pub use self::Api as __PhoxalApiMarker;

                #node_mods

                /// Generated endpoint descriptors, separate from payload
                /// domains so one payload can serve several endpoints.
                pub mod endpoint {
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
            .map(|node| node.expand_endpoint_alias_module(&[]))
            .collect()
    }
}

impl Node {
    /// Emit one version-local domain facade and the descriptors it owns.
    fn expand_module(&self, tree_id: &str, family_prefix: &str, key_prefix: &str) -> TokenStream {
        let name = &self.name;
        let family_path = self.family_path(family_prefix);
        let node_key_prefix = self.key_prefix(key_prefix);

        let mut descriptors = TokenStream::new();
        let mut external_aliases = TokenStream::new();
        let mut external_parents = std::collections::BTreeMap::<String, syn::Path>::new();
        let mut semantic_aliases = std::collections::BTreeMap::<String, syn::Path>::new();

        for topic in &self.topics {
            let key = format!("{tree_id}/{}", topic.leaf.key(&node_key_prefix));
            let endpoint = topic.endpoint_ident();
            let endpoint_name = format!("{tree_id}::{family_path}::{endpoint}");
            let endpoint_contract = format!("{family_path}::{endpoint}");
            let endpoint_kind = topic.endpoint_kind();

            match &topic.kind {
                TopicKind::PubSub(body) => {
                    let payload = &body.path;
                    collect_external_parent(&mut external_parents, payload);
                    register_semantic_alias(&mut semantic_aliases, payload);
                    let delivery_marker = topic.semantic.delivery_marker_trait();
                    let temporal_marker = topic
                        .semantic
                        .semantic_marker_trait()
                        .map(|marker| quote! { impl #marker for #endpoint {} })
                        .unwrap_or_default();
                    descriptors.extend(quote! {
                        #[derive(Clone, Copy, Debug)]
                        pub struct #endpoint;
                        impl ::phoxal_bus::EndpointDescriptor for #endpoint {
                            type Api = self::__PhoxalApiMarker;
                            type Payload = #payload;
                            const NAME: &'static str = #endpoint_name;
                            const VERSION: &'static str = #tree_id;
                            const CONTRACT: &'static str = #endpoint_contract;
                            const TOPIC: &'static str = #key;
                            const KIND: ::phoxal_bus::EndpointKind = #endpoint_kind;
                        }
                        impl #delivery_marker for #endpoint {}
                        #temporal_marker
                    });
                }
                TopicKind::Query { request, response } => {
                    let request = &request.path;
                    let response = &response.path;
                    collect_external_parent(&mut external_parents, request);
                    collect_external_parent(&mut external_parents, response);
                    register_semantic_alias(&mut semantic_aliases, request);
                    register_semantic_alias(&mut semantic_aliases, response);
                    descriptors.extend(quote! {
                        #[derive(Clone, Copy, Debug)]
                        pub struct #endpoint;
                        impl ::phoxal_bus::EndpointDescriptor for #endpoint {
                            type Api = self::__PhoxalApiMarker;
                            type Payload = #request;
                            const NAME: &'static str = #endpoint_name;
                            const VERSION: &'static str = #tree_id;
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

        for body_path in semantic_aliases.values() {
            let Some(last) = body_path.segments.last() else {
                continue;
            };
            let alias = syn::Ident::new(&last.ident.to_string(), last.ident.span());
            external_aliases
                .extend(quote! { #[allow(unused_imports)] pub use #body_path as #alias; });
        }
        let external_parent_imports = external_parents
            .into_values()
            .map(|parent| quote! { #[allow(unused_imports)] pub use #parent::*; })
            .collect::<TokenStream>();
        let child_mods = self
            .children
            .iter()
            .map(|child| child.expand_module(tree_id, &family_path, &node_key_prefix))
            .collect::<TokenStream>();

        quote! {
            pub mod #name {
                /// Version-local domain facade for this endpoint node.

                #[doc(hidden)]
                pub use super::__PhoxalApiMarker;

                #external_parent_imports
                #external_aliases
                #descriptors
                #child_mods
            }
        }
    }

    fn expand_endpoint_alias_module(&self, ancestors: &[syn::Ident]) -> TokenStream {
        let mut path = ancestors.to_vec();
        path.push(self.name.clone());
        let mut aliases = TokenStream::new();
        for topic in &self.topics {
            let endpoint = topic.endpoint_ident();
            let mut descriptor_path = TokenStream::new();
            for _ in 0..path.len() + 1 {
                descriptor_path.extend(quote! { super:: });
            }
            for segment in &path {
                descriptor_path.extend(quote! { #segment :: });
            }
            aliases.extend(quote! { pub use #descriptor_path #endpoint; });
        }
        let children = self
            .children
            .iter()
            .map(|child| child.expand_endpoint_alias_module(&path))
            .collect::<TokenStream>();
        let name = &self.name;
        quote! {
            pub mod #name {
                #aliases
                #children
            }
        }
    }
}

/// Keep the normal domain module visible from a generated version-local facade.
/// Endpoint payload leaves are also individually re-exported, while the module
/// glob makes their supporting domain types available at the same boundary.
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
) {
    let Some(last) = path.segments.last() else {
        return;
    };
    let name = last.ident.to_string();
    let prefer = is_current_version_path(path);
    if aliases
        .get(&name)
        .is_none_or(|existing| prefer && !is_current_version_path(existing))
    {
        aliases.insert(name, path.clone());
    }
}

fn is_current_version_path(path: &syn::Path) -> bool {
    let mut segments = path.segments.iter();
    let prefix = [
        segments.next(),
        segments.next(),
        segments.next(),
        segments.next(),
    ];
    prefix
        .into_iter()
        .zip(["crate", "domains", "v0_2", ""])
        .all(|(segment, expected)| {
            if expected.is_empty() {
                segment.is_some()
            } else {
                segment.is_some_and(|segment| segment.ident == expected)
            }
        })
}
