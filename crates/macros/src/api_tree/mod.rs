//! Expansion for modular contract-family fragments and
//! [`crate::phoxal_protocol`].
//!
//! Both macros declare endpoint topology over normal Rust payload types. The
//! generated output owns endpoint descriptors, topic builders, and endpoint
//! catalogues while family facades re-export their authored payloads. Fragment
//! collection groups fragments into semantic families before this shared
//! generator sees them; protocol mode has one editable, protocol-rooted
//! endpoint tree. Neither mode carries a revision axis: compatibility is the
//! framework train version.

mod bodies;
mod builders;
mod fragments;
mod grammar;
mod manifest;
mod model;

use proc_macro2::TokenStream;
use quote::quote;

use manifest::ManifestFamily;
use model::{MaterializedTree, Protocol};

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
    let mut trees = Vec::new();
    let mut declared = std::collections::BTreeSet::new();
    for protocol in protocols {
        let id = protocol.name.to_string();
        if !declared.insert(id.clone()) {
            return Err(syn::Error::new_spanned(
                &protocol.name,
                format!("duplicate protocol tree `{id}`"),
            ));
        }
        trees.push(MaterializedTree {
            module: protocol.name.clone(),
            doc: format!("Protocol tree `{id}`."),
            id,
            source: None,
            nodes: protocol.nodes.clone(),
        });
    }
    Ok(expand_trees(&trees))
}

/// Emit one endpoint catalogue covering every tree, then each tree's module.
/// Contract families and protocols differ only in how a tree is parsed and
/// identified, never in what its module contains.
fn expand_trees(trees: &[MaterializedTree]) -> TokenStream {
    let manifests: Vec<ManifestFamily> = trees.iter().map(ManifestFamily::of).collect();
    let manifest = ManifestFamily::expand_manifest(&manifests);
    let modules: TokenStream = trees.iter().map(MaterializedTree::expand).collect();
    quote! { #manifest #modules }
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
