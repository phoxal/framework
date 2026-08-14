//! The descriptor side of a generated family: its ordinary payload modules,
//! family marker, and endpoint descriptor tree.
//!
//! The parallel topic-builder tree is emitted by [`super::builders`] and
//! spliced into the same tree module here.

use super::model::{Descriptor, MaterializedTree, Node};
use proc_macro2::TokenStream;
use quote::quote;

impl MaterializedTree {
    /// Emit this tree's module: an API marker, domain facades, endpoint
    /// descriptors, and side-branded topic builders.
    pub(super) fn expand(&self) -> TokenStream {
        let mod_name = &self.module;
        let mut node_mods = TokenStream::new();
        for node in &self.nodes {
            node_mods.extend(node.expand_module());
        }

        let topic_mod = self.expand_topic_module();
        let endpoint_mod = self.expand_endpoint_module();
        let id = mod_name.to_string();
        let module_doc = format!("The `{id}` contract family.");

        quote! {
            #[doc = #module_doc]
            pub mod #mod_name {
                /// Zero-variant marker identifying this contract family.
                #[derive(Clone, Copy, Debug)]
                pub enum Api {}
                impl ::phoxal_bus::ApiFamily for Api {
                    const ID: &'static str = #id;
                }

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
        let family = self.module.to_string();
        self.nodes
            .iter()
            .map(|node| node.expand_endpoint_module(&family, "", ""))
            .collect()
    }
}

impl Node {
    /// Declare the ordinary payload module represented by this path node.
    fn expand_module(&self) -> TokenStream {
        let name = &self.name;
        let child_mods = self
            .children
            .iter()
            .map(Node::expand_module)
            .collect::<TokenStream>();
        if self.children.is_empty() {
            quote! { pub mod #name; }
        } else {
            debug_assert!(
                self.topics.is_empty(),
                "a payload module cannot be both inline and file-backed"
            );
            quote! { pub mod #name { #child_mods } }
        }
    }

    fn expand_endpoint_module(
        &self,
        tree_id: &str,
        family_prefix: &str,
        key_prefix: &str,
    ) -> TokenStream {
        let tree_module = syn::Ident::new(tree_id, proc_macro2::Span::call_site());
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
                    let client_role_marker = topic
                        .descriptor
                        .client_role_marker_trait()
                        .map(|marker| quote! { impl #marker for #endpoint {} })
                        .unwrap_or_default();
                    descriptors.extend(quote! {
                        #[derive(Clone, Copy, Debug)]
                        pub struct #endpoint;
                        impl ::phoxal_bus::EndpointDescriptor for #endpoint {
                            type Api = crate::#tree_module::Api;
                            type Payload = #payload;
                            const NAME: &'static str = #endpoint_name;
                            const FAMILY: &'static str = #tree_id;
                            const CONTRACT: &'static str = #endpoint_contract;
                            const TOPIC: &'static str = #key;
                            const KIND: ::phoxal_bus::EndpointKind = #endpoint_kind;
                        }
                        impl #delivery_marker for #endpoint {}
                        #temporal_marker
                        #client_role_marker
                    });
                }
                Descriptor::Query { request, response } => {
                    let request = &request.path;
                    let response = &response.path;
                    descriptors.extend(quote! {
                        #[derive(Clone, Copy, Debug)]
                        pub struct #endpoint;
                        impl ::phoxal_bus::EndpointDescriptor for #endpoint {
                            type Api = crate::#tree_module::Api;
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
                #descriptors
                #children
            }
        }
    }
}
