//! The contract manifest: the tree's own `#[cfg(test)]` enumeration of every
//! contract it declares.
//!
//! `phoxal-api`'s curation tests read it to assert that each command topic is
//! deliberately classified and each wire key composes as intended, so it is
//! emitted only into test builds and never becomes public API surface.

use proc_macro2::TokenStream;
use quote::quote;

use super::model::{MaterializedTree, Node, TopicKind, TopicRole};

/// One tree (an API revision or a protocol) in the emitted manifest.
pub(super) struct ManifestVersion {
    name: String,
    contracts: Vec<ManifestContract>,
}

struct ManifestContract {
    /// Tree-qualified contract identity, e.g. `"v0.1::drive::Target"` - the
    /// version is part of the name, not a separate axis.
    family: String,
    /// Tree-qualified wire key, e.g. `"v0.1/drive/target"`.
    topic: String,
    /// The declared role, so a check can enumerate every command topic without
    /// parsing names.
    role: TopicRole,
}

impl ManifestVersion {
    /// Enumerate every contract `tree` declares, sorted by `(family, topic)` so
    /// the emitted manifest is order-stable regardless of authoring order.
    pub(super) fn of(tree: &MaterializedTree) -> Self {
        let mut contracts = Vec::new();
        collect(&tree.id, &tree.nodes, "", "", &mut contracts);
        contracts.sort_by(|left, right| {
            left.family
                .cmp(&right.family)
                .then_with(|| left.topic.cmp(&right.topic))
        });
        Self {
            name: tree.id.clone(),
            contracts,
        }
    }

    /// Emit the manifest const and the two record types it is built from.
    pub(super) fn expand_manifest(versions: &[Self]) -> TokenStream {
        let version_entries = versions.iter().map(|version| {
            let name = &version.name;
            let contracts = version.contracts.iter().map(|contract| {
                let family = &contract.family;
                let topic = &contract.topic;
                let role = contract.role.bus_variant();
                quote! {
                    ApiContractManifestContract {
                        family: #family,
                        topic: #topic,
                        role: #role,
                    }
                }
            });
            quote! {
                ApiContractManifestVersion {
                    name: #name,
                    contracts: &[#(#contracts),*],
                }
            }
        });

        quote! {
            /// One generated API version in the contract manifest.
            ///
            /// `#[cfg(test)]`-only: this is the tree's own
            /// self-enumeration, which backs `phoxal-api`'s curation tests (every
            /// command topic is deliberately classified, every wire key composes
            /// as intended). It is available only to test builds.
            #[cfg(test)]
            #[doc(hidden)]
            #[derive(Clone, Copy, Debug, Eq, PartialEq)]
            pub struct ApiContractManifestVersion {
                pub name: &'static str,
                pub contracts: &'static [ApiContractManifestContract],
            }

            /// One generated contract in the contract manifest. `family` is the
            /// version-qualified contract identity; `topic` is its
            /// version-qualified wire key. The name itself is the whole identity.
            /// `#[cfg(test)]`-only - see
            /// [`ApiContractManifestVersion`]'s docs.
            #[cfg(test)]
            #[doc(hidden)]
            #[derive(Clone, Copy, Debug, Eq, PartialEq)]
            pub struct ApiContractManifestContract {
                pub family: &'static str,
                pub topic: &'static str,
                pub role: ::phoxal_bus::TopicRole,
            }

            /// The tree's own enumeration of every contract it declares, used by
            /// `phoxal-api`'s curation tests to assert that each command topic is
            /// deliberately classified and each wire key composes as intended.
            /// `#[cfg(test)]`-only for the two consumers in
            /// `phoxal-api/src/tests.rs`.
            #[cfg(test)]
            #[doc(hidden)]
            pub const API_CONTRACT_MANIFEST: &[ApiContractManifestVersion] = &[#(#version_entries),*];
        }
    }
}

/// Walk `nodes` depth-first, deriving each topic's family path and wire key
/// from exactly the same node-path accessors the body codegen uses, so the
/// manifest and `ContractBody`'s consts cannot drift apart.
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
            match &topic.kind {
                TopicKind::PubSub(body) => {
                    contracts.push(ManifestContract {
                        family: format!("{tree_id}::{family_path}::{body}"),
                        topic: topic_key,
                        role: topic.role,
                    });
                }
                TopicKind::Query { request, response } => {
                    contracts.push(ManifestContract {
                        family: format!("{tree_id}::{family_path}::{request}"),
                        topic: topic_key.clone(),
                        role: topic.role,
                    });
                    contracts.push(ManifestContract {
                        family: format!("{tree_id}::{family_path}::{response}"),
                        topic: topic_key,
                        role: topic.role,
                    });
                }
            }
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
