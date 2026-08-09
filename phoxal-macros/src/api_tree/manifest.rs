//! The endpoint manifest: the tree's own `#[cfg(test)]` enumeration of every
//! endpoint it declares.
//!
//! The manifest is endpoint-owned. Payloads are recorded as shape references,
//! but they do not supply identity: two endpoints may intentionally reuse one
//! payload and still produce two distinct records. Query request and response
//! types likewise remain one endpoint record rather than two body records.

use proc_macro2::TokenStream;
use quote::quote;

use super::model::{MaterializedTree, Node, TopicKind};

/// One tree (an API revision or a protocol) in the emitted manifest.
pub(super) struct ManifestVersion {
    name: String,
    contracts: Vec<ManifestContract>,
    fingerprint: String,
}

struct ManifestContract {
    /// Version-qualified endpoint identity, e.g.
    /// `"v0.1::drive::StateEndpoint"`.
    endpoint: String,
    /// Version-qualified wire key template, e.g. `"v0.1/drive/state"`.
    topic: String,
    /// The payload shape reference for pub/sub endpoints. This is deliberately
    /// optional because query endpoints carry separate request/response shapes.
    payload: Option<String>,
    /// The request shape for a query endpoint.
    request: Option<String>,
    /// The response shape for a query endpoint.
    response: Option<String>,
    /// Fixed endpoint semantic kind.
    kind: &'static str,
    /// Transport family selected by the endpoint kind/source declaration.
    delivery: &'static str,
    /// Token spelling used for the generated enum field.
    kind_tokens: TokenStream,
    /// Token spelling used for the generated enum field.
    delivery_tokens: TokenStream,
}

impl ManifestVersion {
    /// Enumerate every endpoint `tree` declares, sorted by `(endpoint, topic)`
    /// so the emitted manifest is order-stable regardless of authoring order.
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
            fingerprint: fingerprint(&contracts),
            contracts,
        }
    }

    /// Emit the manifest const and the two record types it is built from.
    pub(super) fn expand_manifest(versions: &[Self]) -> TokenStream {
        let version_entries = versions.iter().map(|version| {
            let name = &version.name;
            let fingerprint = &version.fingerprint;
            let contracts = version.contracts.iter().map(|contract| {
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
                ApiContractManifestVersion {
                    name: #name,
                    fingerprint: #fingerprint,
                    contracts: &[#(#contracts),*],
                }
            }
        });

        quote! {
            /// One generated API/protocol tree in the contract manifest.
            #[doc(hidden)]
            #[derive(Clone, Copy, Debug, Eq, PartialEq)]
            pub struct ApiContractManifestVersion {
                pub name: &'static str,
                pub fingerprint: &'static str,
                pub contracts: &'static [ApiContractManifestContract],
            }

            /// One generated endpoint in the contract manifest.
            ///
            /// `endpoint` owns identity. `payload` is populated for pub/sub
            /// endpoints; `request` and `response` are populated for one
            /// request/reply endpoint record. `kind` and `delivery` are kept
            /// separately because the former selects the author-facing time
            /// handle family while the latter selects transport storage.
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

            /// The tree's own enumeration of every endpoint it declares.
            #[doc(hidden)]
            pub const API_CONTRACT_MANIFEST: &[ApiContractManifestVersion] = &[#(#version_entries),*];
        }
    }
}

fn optional_string(value: &Option<String>) -> TokenStream {
    match value {
        Some(value) => quote! { Some(#value) },
        None => quote! { None },
    }
}

/// Walk `nodes`, deriving endpoint identity and wire key from the same accessors
/// used by descriptor codegen. One query contributes one record with both body
/// identities, not two body-owned records.
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
            let kind = topic.endpoint_kind_name();
            let delivery = topic.delivery_family_name();
            let contract = match &topic.kind {
                TopicKind::PubSub(body) => ManifestContract {
                    endpoint: endpoint_name,
                    topic: topic_key,
                    payload: Some(body.identity()),
                    request: None,
                    response: None,
                    kind,
                    delivery,
                    kind_tokens,
                    delivery_tokens,
                },
                TopicKind::Query { request, response } => ManifestContract {
                    endpoint: endpoint_name,
                    topic: topic_key,
                    payload: None,
                    request: Some(request.identity()),
                    response: Some(response.identity()),
                    kind,
                    delivery,
                    kind_tokens,
                    delivery_tokens,
                },
            };
            contracts.push(contract);
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

/// FNV-1a is intentionally implemented here instead of depending on a hash
/// crate: this is a deterministic source fingerprint, not a security digest.
/// Every endpoint identity, wire key, payload/query shape, endpoint kind, and
/// delivery family participates in the input.
fn fingerprint(contracts: &[ManifestContract]) -> String {
    let mut hash = 0xcbf29ce484222325u64;
    for contract in contracts {
        feed(&mut hash, "endpoint");
        feed(&mut hash, &contract.endpoint);
        feed(&mut hash, "topic");
        feed(&mut hash, &contract.topic);
        feed(&mut hash, "payload");
        feed(&mut hash, contract.payload.as_deref().unwrap_or(""));
        feed(&mut hash, "request");
        feed(&mut hash, contract.request.as_deref().unwrap_or(""));
        feed(&mut hash, "response");
        feed(&mut hash, contract.response.as_deref().unwrap_or(""));
        feed(&mut hash, "kind");
        feed(&mut hash, contract.kind);
        feed(&mut hash, "delivery");
        feed(&mut hash, contract.delivery);
    }
    format!("fnv1a64-{hash:016x}")
}

fn feed(hash: &mut u64, value: &str) {
    for byte in value.bytes().chain([0]) {
        *hash ^= u64::from(byte);
        *hash = hash.wrapping_mul(0x100000001b3);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn contract(
        payload: Option<&str>,
        request: Option<&str>,
        response: Option<&str>,
        kind: &'static str,
        delivery: &'static str,
    ) -> ManifestContract {
        ManifestContract {
            endpoint: "tree::node::Endpoint".to_string(),
            topic: "tree/node/topic".to_string(),
            payload: payload.map(str::to_owned),
            request: request.map(str::to_owned),
            response: response.map(str::to_owned),
            kind,
            delivery,
            kind_tokens: quote! {},
            delivery_tokens: quote! {},
        }
    }

    #[test]
    fn fingerprint_includes_endpoint_kind_delivery_and_complete_shape() {
        let base = vec![contract(
            Some("crate::Payload"),
            None,
            None,
            "State",
            "State",
        )];
        let variants = [
            vec![contract(
                Some("crate::Payload"),
                None,
                None,
                "Sample",
                "Sample",
            )],
            vec![contract(
                Some("crate::Payload"),
                None,
                None,
                "State",
                "Sample",
            )],
            vec![contract(
                Some("crate::OtherPayload"),
                None,
                None,
                "State",
                "State",
            )],
            vec![contract(
                None,
                Some("crate::Request"),
                Some("crate::Response"),
                "Query",
                "Query",
            )],
        ];

        for variant in variants {
            assert_ne!(
                fingerprint(&base),
                fingerprint(&variant),
                "endpoint kind, delivery family, and request/payload shape must affect the fingerprint"
            );
        }
    }
}
