//! Expansion for modular contract-family fragments.
//!
//! Fragments declare endpoint topology over normal Rust payload types. The
//! generated output owns endpoint descriptors, topic builders, and endpoint
//! catalogues while family facades re-export their authored payloads. Fragment
//! collection groups fragments into semantic families before this generator
//! sees them. There is no revision axis: compatibility is the framework train
//! version.

mod bodies;
mod builders;
mod fragments;
mod grammar;
mod manifest;
mod model;

use proc_macro2::TokenStream;
use quote::quote;

use manifest::ManifestFamily;
use model::MaterializedTree;

pub(crate) use fragments::{
    expand_fragment, expand_fragment_group, expand_group_collector, expand_materialized,
    expand_tree,
};

/// Emit one endpoint catalogue covering every family tree, then each tree's
/// module.
fn expand_trees(trees: &[MaterializedTree]) -> TokenStream {
    let manifests: Vec<ManifestFamily> = trees.iter().map(ManifestFamily::of).collect();
    let manifest = ManifestFamily::expand_manifest(&manifests);
    let modules: TokenStream = trees.iter().map(MaterializedTree::expand).collect();
    quote! { #manifest #modules }
}
