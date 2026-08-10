//! Generated endpoint catalogue for materialized family and protocol trees.

use proc_macro2::TokenStream;
use quote::quote;

use super::model::{MaterializedTree, Node, TopicKind};

/// One tree (a contract family or a protocol) in the emitted manifest.
pub(super) struct ManifestFamily {
    name: String,
    contracts: Vec<ManifestContract>,
}

struct ManifestContract {
    endpoint: String,
    topic: String,
    payload: Option<String>,
    request: Option<String>,
    response: Option<String>,
    kind_tokens: TokenStream,
    delivery_tokens: TokenStream,
}

impl ManifestFamily {
    /// Enumerate every endpoint, sorted independently of authoring order.
    pub(super) fn of(tree: &MaterializedTree) -> Self {
        let mut contracts = Vec::new();
        collect(&tree.id, &tree.nodes, "", "", &mut contracts);
        contracts.sort_by(|left, right| {
            left.endpoint
                .cmp(&right.endpoint)
                .then_with(|| left.topic.cmp(&right.topic))
        });
        Self {
            name: tree.id.clone(),
            contracts,
        }
    }

    pub(super) fn expand_manifest(families: &[Self]) -> TokenStream {
        let family_entries = families.iter().map(|family| {
            let name = &family.name;
            let contracts = family.contracts.iter().map(|contract| {
                let endpoint = &contract.endpoint;
                let topic = &contract.topic;
                let payload = optional_string(&contract.payload);
                let request = optional_string(&contract.request);
                let response = optional_string(&contract.response);
                let kind = &contract.kind_tokens;
                let delivery = &contract.delivery_tokens;
                quote! {
                    ApiContractManifestContract {
                        endpoint: #endpoint,
                        topic: #topic,
                        payload: #payload,
                        request: #request,
                        response: #response,
                        kind: #kind,
                        delivery: #delivery,
                    }
                }
            });
            quote! {
                ApiContractManifestFamily {
                    name: #name,
                    contracts: &[#(#contracts),*],
                }
            }
        });

        quote! {
            /// One generated family/protocol tree in the endpoint catalogue.
            #[doc(hidden)]
            #[derive(Clone, Copy, Debug, Eq, PartialEq)]
            pub struct ApiContractManifestFamily {
                pub name: &'static str,
                pub contracts: &'static [ApiContractManifestContract],
            }

            /// One generated endpoint and its transport contract.
            #[doc(hidden)]
            #[derive(Clone, Copy, Debug, Eq, PartialEq)]
            pub struct ApiContractManifestContract {
                pub endpoint: &'static str,
                pub topic: &'static str,
                pub payload: Option<&'static str>,
                pub request: Option<&'static str>,
                pub response: Option<&'static str>,
                pub kind: ::phoxal_bus::EndpointKind,
                pub delivery: ::phoxal_bus::DeliveryFamily,
            }

            /// Every endpoint declared by the materialized trees.
            #[doc(hidden)]
            pub const API_CONTRACT_MANIFEST: &[ApiContractManifestFamily] = &[#(#family_entries),*];
        }
    }
}

fn optional_string(value: &Option<String>) -> TokenStream {
    match value {
        Some(value) => quote! { Some(#value) },
        None => quote! { None },
    }
}

fn collect(
    tree_id: &str,
    nodes: &[Node],
    family_prefix: &str,
    key_prefix: &str,
    contracts: &mut Vec<ManifestContract>,
) {
    for node in nodes {
        let family_path = node.family_path(family_prefix);
        let node_key_prefix = node.key_prefix(key_prefix);

        for topic in &node.topics {
            let topic_key = format!("{tree_id}/{}", topic.leaf.key(&node_key_prefix));
            let endpoint = topic.endpoint_ident();
            let endpoint_name = format!("{tree_id}::{family_path}::{endpoint}");
            let kind_tokens = topic.endpoint_kind();
            let delivery_tokens = topic.delivery_family();
            contracts.push(match &topic.kind {
                TopicKind::PubSub(body) => ManifestContract {
                    endpoint: endpoint_name,
                    topic: topic_key,
                    payload: Some(body.identity()),
                    request: None,
                    response: None,
                    kind_tokens,
                    delivery_tokens,
                },
                TopicKind::Query { request, response } => ManifestContract {
                    endpoint: endpoint_name,
                    topic: topic_key,
                    payload: None,
                    request: Some(request.identity()),
                    response: Some(response.identity()),
                    kind_tokens,
                    delivery_tokens,
                },
            });
        }

        collect(
            tree_id,
            &node.children,
            &family_path,
            &node_key_prefix,
            contracts,
        );
    }
}
