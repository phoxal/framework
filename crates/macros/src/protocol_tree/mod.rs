//! Expansion for the framework's complete protocol catalogue.
//!
//! One declaration binds endpoint topology to ordinary Rust payload modules.
//! The generated output declares those modules directly and adds endpoint
//! descriptors, topic builders, and compatibility records. There is no
//! collector chain and no revision axis.

mod bodies;
mod builders;
mod catalogue;
mod grammar;
mod manifest;
mod model;

use proc_macro2::TokenStream;

use manifest::ManifestFamily;
use model::MaterializedTree;

pub(crate) use catalogue::expand_tree;

/// Emit one endpoint catalogue covering every family tree, then each tree's
/// module.
fn expand_trees(trees: &[MaterializedTree]) -> TokenStream {
    let manifests: Vec<ManifestFamily> = trees.iter().map(ManifestFamily::of).collect();
    let mut output = ManifestFamily::expand_manifest(&manifests);
    output.extend(trees.iter().map(MaterializedTree::expand));
    output
}
