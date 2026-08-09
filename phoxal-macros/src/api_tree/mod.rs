//! Expansion for modular Robot API fragments and [`crate::phoxal_protocol`].
//!
//! Both macros declare endpoint topology over normal Rust payload types. The
//! generated output owns endpoint descriptors, topic builders, and endpoint
//! catalogues while revision facades re-export their authored payloads.
//! Fragment collection materializes Robot API revisions before this shared
//! generator sees them; protocol mode has one editable, protocol-rooted
//! endpoint tree without a revision axis.

mod bodies;
mod builders;
mod fragments;
mod grammar;
mod manifest;
mod model;

use proc_macro2::TokenStream;
use quote::{format_ident, quote};
use syn::Ident;

use manifest::ManifestVersion;
use model::{ConcreteVersion, MaterializedTree, Protocol};

pub(crate) use fragments::{
    expand_fragment, expand_fragment_group, expand_group_collector, expand_materialized,
    expand_tree,
};

/// Expand only protocol trees. Keeping this entry point distinct from
/// [`expand_api`] makes the protocol-mode boundary explicit at the call site.
pub fn expand_protocol(input: TokenStream) -> syn::Result<TokenStream> {
    let protocols: grammar::ProtocolInput = syn::parse2(input)?;
    expand_protocols(&protocols.0)
}

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
        let tree = MaterializedTree {
            module: protocol.name.clone(),
            doc: format!("Protocol tree `{id}`."),
            id,
            source: None,
            nodes: protocol.nodes.clone(),
        };
        manifests.push(ManifestVersion::of(&tree));
        output.extend(tree.expand());
    }
    let manifest = ManifestVersion::expand_manifest(&manifests);
    Ok(quote! { #manifest #output })
}

fn expand_api(
    versions: &[ConcreteVersion],
    latest: &Ident,
    source: &syn::Path,
) -> syn::Result<TokenStream> {
    let mut output = TokenStream::new();
    let mut manifests = Vec::new();
    let mut declared = std::collections::BTreeSet::new();
    for version in versions {
        let name = version.name.to_string();
        if !declared.insert(name) {
            return Err(syn::Error::new_spanned(
                &version.name,
                format!("duplicate API revision `{}`", version.name),
            ));
        }
        let tree = MaterializedTree {
            module: version.name.clone(),
            id: version.wire_id.clone(),
            doc: format!("Concrete API revision `{}`.", version.wire_id),
            source: Some(source.clone()),
            nodes: version.nodes.clone(),
        };
        manifests.push(ManifestVersion::of(&tree));
        output.extend(tree.expand());
    }
    if !declared.contains(&latest.to_string()) {
        return Err(syn::Error::new_spanned(
            latest,
            "`latest` must name a declared concrete API revision",
        ));
    }
    let manifest = ManifestVersion::expand_manifest(&manifests);
    let catalogue = {
        let declarations = versions
            .iter()
            .map(|version| format_ident!("V{}", version.name.to_string().trim_start_matches('v')));
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
                let major = version.major;
                let minor = version.minor;
                quote! { Self::#variant => ::phoxal_runtime_contract::version::RobotApiVersion::new(#major, #minor) }
            });
        let latest_variant = format_ident!("V{}", latest.to_string().trim_start_matches('v'));
        quote! {
            /// One exact Robot API revision implemented by this crate.
            #[allow(non_camel_case_types)]
            #[derive(Clone, Copy, Debug, Eq, PartialEq, Ord, PartialOrd, Hash)]
            pub enum RobotApi { #(#declarations),* }
            impl RobotApi {
                /// The canonical process-boundary identity.
                pub const fn as_str(self) -> &'static str { match self { #(#process_ids),* } }
                /// The open process-contract value for this implemented revision.
                pub const fn version(self) -> ::phoxal_runtime_contract::version::RobotApiVersion {
                    match self { #(#versions_by_value),* }
                }
                /// Select the exact implemented revision, if this crate contains it.
                #[must_use]
                pub fn from_version(version: ::phoxal_runtime_contract::version::RobotApiVersion) -> Option<Self> {
                    Self::ALL.iter().copied().find(|candidate| candidate.version() == version)
                }
                /// Every exact revision implemented by this crate.
                pub const ALL: &'static [Self] = &[#(#names),*];
                /// The revision selected by the release train facade.
                pub const LATEST: Self = Self::#latest_variant;
            }

            /// An open Robot API version that this generated catalogue does not implement.
            #[derive(Clone, Copy, Debug, Eq, PartialEq)]
            pub struct UnsupportedRobotApi {
                version: ::phoxal_runtime_contract::version::RobotApiVersion,
            }

            impl UnsupportedRobotApi {
                /// The exact version advertised by the remote robot.
                #[must_use]
                pub const fn version(self) -> ::phoxal_runtime_contract::version::RobotApiVersion {
                    self.version
                }
            }

            impl ::std::fmt::Display for UnsupportedRobotApi {
                fn fmt(&self, formatter: &mut ::std::fmt::Formatter<'_>) -> ::std::fmt::Result {
                    write!(formatter, "unsupported Robot API `{}`", self.version)
                }
            }

            impl ::std::error::Error for UnsupportedRobotApi {}

            impl ::std::convert::TryFrom<::phoxal_runtime_contract::version::RobotApiVersion> for RobotApi {
                type Error = UnsupportedRobotApi;

                fn try_from(version: ::phoxal_runtime_contract::version::RobotApiVersion) -> Result<Self, Self::Error> {
                    Self::from_version(version).ok_or(UnsupportedRobotApi { version })
                }
            }
        }
    };
    Ok(quote! {
        #manifest
        #output
        #catalogue
        #[allow(unused_imports)]
        pub use #latest as latest;
    })
}

#[cfg(test)]
mod tests {
    use super::expand_protocol;
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
}
