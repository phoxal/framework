//! Expansion for [`crate::phoxal_api`] and [`crate::phoxal_protocol`].
//!
//! Both macros declare endpoint topology over normal Rust payload types. The
//! generated output owns endpoint descriptors, topic builders, and contract
//! manifests; payload definitions remain in their domain modules. API mode has
//! version materialization and one selected latest revision. Protocol mode has
//! one editable, protocol-rooted endpoint tree without a revision axis.

mod bodies;
mod builders;
mod grammar;
mod manifest;
mod model;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Ident;

use grammar::{PROTOCOL_HAS_NO_DELTAS, VERSION_HAS_NO_PARENT};
use manifest::ManifestVersion;
use model::{MaterializedTree, Node, Protocol, Version};

/// Expand only a robot API tree. Protocol declarations deliberately have a
/// separate proc-macro entry point so an API source cannot acquire
/// protocol-mode semantics by accident.
pub fn expand_api(input: TokenStream) -> syn::Result<TokenStream> {
    match syn::parse2(input)? {
        ApiTree::Api { versions, latest } => ApiTree::expand_api(&versions, &latest, true),
        ApiTree::Protocols(_) => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "`phoxal_api!` accepts only `version` revisions; use `phoxal_protocol!` for protocol trees",
        )),
    }
}

/// Expand only protocol trees. Keeping this entry point distinct from
/// [`expand_api`] makes the protocol-mode boundary explicit at the call site.
pub fn expand_protocol(input: TokenStream) -> syn::Result<TokenStream> {
    match syn::parse2(input)? {
        ApiTree::Protocols(protocols) => ApiTree::expand_protocols(&protocols),
        ApiTree::Api { .. } => Err(syn::Error::new(
            proc_macro2::Span::call_site(),
            "`phoxal_protocol!` accepts only `protocol` trees; use `phoxal_api!` for version revisions",
        )),
    }
}

/// One semantic declaration, in exactly one mode.
enum ApiTree {
    Api {
        versions: Vec<Version>,
        latest: Ident,
    },
    Protocols(Vec<Protocol>),
}

impl ApiTree {
    fn expand_protocols(protocols: &[Protocol]) -> syn::Result<TokenStream> {
        let mut output = TokenStream::new();
        let mut manifests = Vec::new();
        let mut declared = std::collections::BTreeSet::new();
        for protocol in protocols {
            let id = protocol.name.to_string();
            if !declared.insert(id.clone()) {
                return Err(syn::Error::new_spanned(
                    &protocol.name,
                    format!("duplicate protocol tree `{id}`"),
                ));
            }
            Node::reject_delta_forms(&protocol.nodes, &[], PROTOCOL_HAS_NO_DELTAS)?;
            let tree = MaterializedTree {
                module: protocol.name.clone(),
                doc: format!("Protocol tree `{id}`."),
                id,
                nodes: protocol.nodes.clone(),
            };
            manifests.push(ManifestVersion::of(&tree));
            output.extend(tree.expand());
        }
        let manifest = ManifestVersion::expand_manifest(&manifests);
        Ok(quote! { #manifest #output })
    }

    fn expand_api(
        versions: &[Version],
        latest: &Ident,
        emit_catalogue: bool,
    ) -> syn::Result<TokenStream> {
        let mut output = TokenStream::new();
        let mut manifests = Vec::new();
        let mut materialized = std::collections::BTreeMap::<String, Vec<Node>>::new();
        for version in versions {
            let name = version.name.to_string();
            if materialized.contains_key(&name) {
                return Err(syn::Error::new_spanned(
                    &version.name,
                    format!("duplicate API revision `{}`", version.name),
                ));
            }
            if emit_catalogue && version.latest && !version.latest_prefix {
                return Err(syn::Error::new_spanned(
                    &version.name,
                    "strict `phoxal_api!` requires `latest version vM.m ...`",
                ));
            }
            let nodes = match &version.parent {
                Some(parent) => {
                    let base = materialized.get(&parent.to_string()).ok_or_else(|| {
                        syn::Error::new_spanned(
                            parent,
                            "an `extends` parent must be a concrete revision declared earlier",
                        )
                    })?;
                    version.materialize_from(base, parent)?
                }
                None => {
                    Node::reject_delta_forms(
                        &version.nodes,
                        &version.removals,
                        VERSION_HAS_NO_PARENT,
                    )?;
                    version.nodes.clone()
                }
            };
            let tree = MaterializedTree {
                module: version.name.clone(),
                id: version.wire_id.clone(),
                doc: format!("Concrete API revision `{}`.", version.wire_id),
                nodes,
            };
            manifests.push(ManifestVersion::of(&tree));
            output.extend(tree.expand());
            materialized.insert(name, tree.nodes);
        }
        if !materialized.contains_key(&latest.to_string()) {
            return Err(syn::Error::new_spanned(
                latest,
                "`latest` must name a declared concrete API revision",
            ));
        }
        let manifest = ManifestVersion::expand_manifest(&manifests);
        let catalogue = emit_catalogue.then(|| {
            let declarations = versions.iter().map(|version| {
                format_ident!("V{}", version.name.to_string().trim_start_matches('v'))
            });
            let names = versions.iter().map(|version| {
                let variant = format_ident!("V{}", version.name.to_string().trim_start_matches('v'));
                quote! { RobotApi::#variant }
            });
            let process_ids = versions.iter().map(|version| {
                let variant = format_ident!("V{}", version.name.to_string().trim_start_matches('v'));
                let process_id = format!("phoxal/robot-api/{}", version.wire_id);
                quote! { Self::#variant => #process_id }
            });
            let versions_by_value = versions.iter().map(|version| {
                let variant = format_ident!("V{}", version.name.to_string().trim_start_matches('v'));
                let Some((major, minor)) = version.wire_id[1..].split_once('.') else {
                    unreachable!("validated revision has a dotted wire id")
                };
                let major: u16 = major
                    .parse()
                    .unwrap_or_else(|_| unreachable!("validated major revision"));
                let minor: u16 = minor
                    .parse()
                    .unwrap_or_else(|_| unreachable!("validated minor revision"));
                quote! { Self::#variant => ::phoxal_runtime_contract::version::RobotApiVersion::new(#major, #minor) }
            });
            let latest_variant = format_ident!("V{}", latest.to_string().trim_start_matches('v'));
            quote! {
                #[doc(hidden)]
                #[allow(non_camel_case_types)]
                #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
                pub enum RobotApi { #(#declarations),* }
                impl RobotApi {
                    pub const fn as_str(self) -> &'static str { match self { #(#process_ids),* } }
                    pub const fn version(self) -> ::phoxal_runtime_contract::version::RobotApiVersion {
                        match self { #(#versions_by_value),* }
                    }
                    pub fn from_version(version: ::phoxal_runtime_contract::version::RobotApiVersion) -> Option<Self> {
                        Self::ALL.iter().copied().find(|candidate| candidate.version() == version)
                    }
                    pub const ALL: &'static [Self] = &[#(#names),*];
                    pub const LATEST: Self = Self::#latest_variant;
                }
            }
        });
        Ok(quote! {
            #manifest
            #output
            #catalogue
            pub use #latest as latest;
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{expand_api, expand_protocol};
    use proc_macro2::TokenStream;
    use quote::quote;

    fn compact(tokens: TokenStream) -> String {
        tokens
            .to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn api_descriptors_and_builders_share_versioned_keys() {
        let expanded = compact(
            expand_api(quote! {
                latest version v0.2 { drive { command target: Setpoint<crate::Target>; } }
            })
            .expect("API expands"),
        );
        assert!(expanded.contains("const TOPIC : & 'static str = \"v0.2/drive/target\""));
        assert!(expanded.contains("Topic :: new_static (\"v0.2/drive/target\")"));
        assert!(expanded.contains("EndpointKind :: Setpoint"));
    }

    #[test]
    fn revisions_materialize_descriptor_deltas() {
        let input = quote! {
            version v0.1 { data { topic state: State<crate::State>; } }
            latest version v0.2 extends v0.1 {
                data { replace topic state: Sample<crate::Sample>; }
            }
        };
        let first = compact(expand_api(input.clone()).expect("first expansion"));
        assert_eq!(first, compact(expand_api(input).expect("second expansion")));
        assert!(first.contains("v0.2/data/state"));
        assert!(first.contains("EndpointKind :: Sample"));
    }

    #[test]
    fn protocols_keep_their_own_root() {
        let expanded = compact(
            expand_protocol(quote! {
                protocol supervisor { logs { topic self: Stream<crate::Log>; } }
            })
            .expect("protocol expands"),
        );
        assert!(expanded.contains("supervisor/logs"));
        assert!(expanded.contains("const ID : & 'static str = \"supervisor\""));
    }

    #[test]
    fn authored_bodies_are_rejected_at_the_grammar_boundary() {
        let error = expand_api(quote! {
            latest version v0.2 { drive { struct Target; } }
        })
        .expect_err("tree-local payloads are unsupported");
        assert!(error.to_string().contains("ordinary Rust domain types"));
    }
}
