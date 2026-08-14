//! Generated endpoint catalogue for materialized contract families.
//!
//! Two documents come out of the same collected endpoint list. The manifest is
//! the const catalogue in-crate tests and tooling read; the contract surface is
//! the machine-readable statement of the same topology for compatibility CI,
//! carrying each endpoint's wire shapes instead of its type names. Building
//! both from one traversal is what keeps them from ever describing different
//! endpoint sets.

use proc_macro2::TokenStream;
use quote::quote;

use super::model::{BodyPath, Descriptor, MaterializedTree, Node};

/// One contract family in the emitted manifest.
pub(super) struct ManifestFamily {
    name: String,
    contracts: Vec<ManifestContract>,
}

struct ManifestContract {
    endpoint: String,
    topic: String,
    bodies: ManifestBodies,
    kind_tokens: TokenStream,
    delivery_tokens: TokenStream,
}

/// The bodies one endpoint carries.
///
/// This mirrors the source grammar's own split rather than three independent
/// options, so "a pub/sub endpoint has no request" is a fact of the type and
/// neither emitted document has a case that cannot happen.
enum ManifestBodies {
    PubSub(BodyPath),
    Query {
        request: BodyPath,
        response: BodyPath,
    },
}

impl ManifestBodies {
    fn payload(&self) -> Option<&BodyPath> {
        match self {
            Self::PubSub(body) => Some(body),
            Self::Query { .. } => None,
        }
    }

    fn request(&self) -> Option<&BodyPath> {
        match self {
            Self::PubSub(_) => None,
            Self::Query { request, .. } => Some(request),
        }
    }

    fn response(&self) -> Option<&BodyPath> {
        match self {
            Self::PubSub(_) => None,
            Self::Query { response, .. } => Some(response),
        }
    }
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
        let surface = Self::expand_contract_surface(families);
        let family_entries = families.iter().map(|family| {
            let name = &family.name;
            let contracts = family.contracts.iter().map(|contract| {
                let endpoint = &contract.endpoint;
                let topic = &contract.topic;
                let payload = optional_identity(contract.bodies.payload());
                let request = optional_identity(contract.bodies.request());
                let response = optional_identity(contract.bodies.response());
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
            /// One generated contract family in the endpoint catalogue.
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

            #surface
        }
    }

    /// Emit the crate's contract surface: one record per endpoint, carrying the
    /// wire schema of every body it names.
    ///
    /// The schemas are composed at call time rather than in a const, because a
    /// `WireSchema` is an owned tree. What has to be deterministic is the
    /// rendered output, and that follows from the record set and the canonical
    /// rendering, not from when the tree is built.
    fn expand_contract_surface(families: &[Self]) -> TokenStream {
        let records = families.iter().flat_map(|family| {
            let name = &family.name;
            family.contracts.iter().map(move |contract| {
                let topic = &contract.topic;
                let kind = &contract.kind_tokens;
                let delivery = &contract.delivery_tokens;
                match &contract.bodies {
                    ManifestBodies::PubSub(payload) => {
                        let payload = wire_schema_of(payload);
                        quote! {
                            ::phoxal_runtime_contract::contract_surface::ContractRecord::topic(
                                #name, #topic, #kind.as_str(), #delivery.as_str(), #payload,
                            )
                        }
                    }
                    ManifestBodies::Query { request, response } => {
                        let request = wire_schema_of(request);
                        let response = wire_schema_of(response);
                        quote! {
                            ::phoxal_runtime_contract::contract_surface::ContractRecord::query(
                                #name, #topic, #kind.as_str(), #delivery.as_str(), #request, #response,
                            )
                        }
                    }
                }
            })
        });

        quote! {
            /// The contract surface this crate owns: every declared endpoint,
            /// with the wire shape of every body it carries.
            ///
            /// Not public API. It exists so compatibility CI can read the
            /// declared endpoint topology out of the crate that declares it.
            #[doc(hidden)]
            pub mod __compat {
                /// The canonical rendering of this crate's contract surface.
                #[must_use]
                pub fn contract_surface() -> ::std::string::String {
                    ::phoxal_runtime_contract::contract_surface::ContractSurface::new(
                        ::std::vec![#(#records),*],
                    )
                    .canonical_json()
                }
            }
        }
    }
}

fn wire_schema_of(body: &BodyPath) -> TokenStream {
    let path = &body.path;
    quote! {
        <#path as ::phoxal_runtime_contract::wire_schema::DescribeWire>::wire_schema()
    }
}

fn optional_identity(value: Option<&BodyPath>) -> TokenStream {
    match value {
        Some(value) => {
            let identity = value.identity();
            quote! { Some(#identity) }
        }
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
            contracts.push(ManifestContract {
                endpoint: endpoint_name,
                topic: topic_key,
                bodies: match &topic.descriptor {
                    Descriptor::State(body)
                    | Descriptor::Sample(body)
                    | Descriptor::Event(body)
                    | Descriptor::Setpoint(body)
                    | Descriptor::Stream { body, .. } => ManifestBodies::PubSub(body.clone()),
                    Descriptor::Query { request, response } => ManifestBodies::Query {
                        request: request.clone(),
                        response: response.clone(),
                    },
                },
                kind_tokens,
                delivery_tokens,
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
