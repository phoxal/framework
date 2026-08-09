//! The wire-body side of a generated tree: the tree module and its `Api`
//! marker, one `pub mod` per node holding that node's tree-local bodies, and
//! the `ContractBody` impls that bind each body to its wire key.
//!
//! The parallel topic-builder tree is emitted by [`super::builders`] and
//! spliced into the same tree module here.

use proc_macro2::TokenStream;
use quote::quote;
use syn::{Ident, ItemStruct};

use super::model::{MaterializedTree, Node, TopicKind, TypeItem};

impl MaterializedTree {
    /// Emit this tree's module: the zero-variant `Api` marker that keeps one
    /// tree's bodies from being mistaken for another's, one node module per
    /// top-level node, and the api-local `topic` builder module.
    pub(super) fn expand(&self) -> TokenStream {
        let mod_name = &self.module;

        // Node modules (types + ContractBody impls), recursive. The family
        // prefix (`::`-joined node names) and the key prefix (`/`-joined `name`
        // or `name/{var}` segments) are threaded down the walk, as is the
        // tree's own identity so every emitted `TOPIC` is qualified by it: the
        // revision - or, in protocol mode, the protocol name - is folded into
        // the wire key, so two trees can never collide.
        let mut node_mods = TokenStream::new();
        for node in &self.nodes {
            node_mods.extend(node.expand_module(&self.id, "", ""));
        }

        let topic_mod = self.expand_topic_module();
        let module_doc = &self.doc;
        let id = &self.id;

        quote! {
            #[doc = #module_doc]
            pub mod #mod_name {
                /// Zero-variant marker identifying this tree: the API revision
                /// in `version` mode, the protocol in `protocol` mode.
                #[derive(Clone, Copy, Debug)]
                pub enum Api {}
                impl ::phoxal_bus::ApiVersion for Api {
                    const ID: &'static str = #id;
                }

                // Self-contained absolute-path anchor, position-independent
                // regardless of where `phoxal_api_tree!` is invoked (crate root
                // or a nested test-fixture module). Every node module and every
                // topic-builder module below - no matter how deep the tree -
                // re-exports this ONE hop from its own parent, so any of them
                // reaches `Api` through a purely local `self::__PhoxalApiMarker`
                // that never needs a supers count tied to its nesting depth.
                #[doc(hidden)]
                pub use self::Api as __PhoxalApiMarker;

                #node_mods

                #topic_mod
            }
        }
    }
}

impl Node {
    /// Emit a `pub mod <name>` for this node. The module carries the node's
    /// types, the `ContractBody` impls for its topics, and - recursively - its
    /// child node modules. Dynamic variables never appear in the module path:
    /// they are topic params, not type-path segments.
    ///
    /// `tree_id` is the tree's identity (the dotted revision `"v0.1"`, or a
    /// protocol name), threaded down so every emitted `TOPIC` carries it.
    /// `family_prefix` is the `::`-joined ancestor node names (empty at the
    /// root); `key_prefix` is the `/`-joined ancestor key segments (`name` or
    /// `name/{var}`, empty at the root). The node appends its own contribution
    /// to each.
    fn expand_module(&self, tree_id: &str, family_prefix: &str, key_prefix: &str) -> TokenStream {
        let name = &self.name;

        // Generated wire bodies remain convenient to construct and copy for
        // the current API. `ContractBody` itself does not require `Clone`, so
        // custom large bodies can opt out; retained transport state is shared
        // through `Arc`.
        let derives =
            quote!(#[derive(Clone, Debug, PartialEq, ::serde::Serialize, ::serde::Deserialize)]);

        let family_path = self.family_path(family_prefix);
        let node_key_prefix = self.key_prefix(key_prefix);

        let mut types = TokenStream::new();
        for ty in &self.types {
            match &ty.item {
                TypeItem::Struct(item) => {
                    let item = with_pub_fields(item.clone());
                    types.extend(quote! { #derives #item });
                }
                TypeItem::Enum(item) => {
                    types.extend(quote! { #derives #item });
                }
            }
        }

        let mut impls = TokenStream::new();
        for topic in &self.topics {
            // The tree-qualified wire key: folding the tree's identity in here
            // is what makes two trees' contracts physically distinct Zenoh keys.
            let key = format!("{tree_id}/{}", topic.leaf.key(&node_key_prefix));
            let role = topic.role.bus_variant();
            let delivery = topic.delivery.map_or_else(
                || topic.role.bus_delivery(),
                |delivery| delivery.bus_variant(),
            );
            // The tree-qualified type-path name: the tree identity, then the
            // `::`-joined node path (vars excluded), then the body's own
            // PascalCase leaf. This is the exact same identity the contract
            // manifest computes for `family`, kept in lockstep by construction
            // (both derive it from `Node::family_path` and the tree id).
            // `VERSION`/`CONTRACT` are the split form of the same identity:
            // `VERSION` is just `tree_id`, `CONTRACT` is `family_path::body`
            // with the tree identity dropped - `NAME == VERSION + "::" +
            // CONTRACT` by construction.
            let tree_id = tree_id.to_string();
            let name_for = |body: &Ident| format!("{tree_id}::{family_path}::{body}");
            let contract_for = |body: &Ident| format!("{family_path}::{body}");
            match &topic.kind {
                TopicKind::PubSub(body) => {
                    let name = name_for(body);
                    let contract = contract_for(body);
                    // The role rides as an inherent `#[doc(hidden)] pub const ROLE` on
                    // the body: additive surface that does not touch `ContractBody`.
                    // The side-branded builders enforce owner/client; the
                    // temporal-role marker enforces which publisher handle - and so
                    // which robot time - this body admits.
                    let marker = topic
                        .role
                        .marker_trait()
                        .map(|marker| quote! { impl #marker for #body {} });
                    let delivery_marker = topic.delivery.map_or_else(
                        || topic.role.delivery_marker_trait(),
                        |delivery| delivery.marker_trait(),
                    );
                    impls.extend(quote! {
                        impl ::phoxal_bus::ContractBody for #body {
                            type Api = self::__PhoxalApiMarker;
                            const NAME: &'static str = #name;
                            const VERSION: &'static str = #tree_id;
                            const CONTRACT: &'static str = #contract;
                            const TOPIC: &'static str = #key;
                            const ROLE: ::phoxal_bus::TopicRole = #role;
                            const DELIVERY: ::phoxal_bus::DeliveryFamily = #delivery;
                        }
                        impl #delivery_marker for #body {}
                        #marker
                    });
                }
                TopicKind::Query { request, response } => {
                    let request_name = name_for(request);
                    let response_name = name_for(response);
                    let request_contract = contract_for(request);
                    let response_contract = contract_for(response);
                    impls.extend(quote! {
                        impl ::phoxal_bus::ContractBody for #request {
                            type Api = self::__PhoxalApiMarker;
                            const NAME: &'static str = #request_name;
                            const VERSION: &'static str = #tree_id;
                            const CONTRACT: &'static str = #request_contract;
                            const TOPIC: &'static str = #key;
                            const ROLE: ::phoxal_bus::TopicRole = #role;
                            const DELIVERY: ::phoxal_bus::DeliveryFamily = #delivery;
                        }
                        impl ::phoxal_bus::ContractBody for #response {
                            type Api = self::__PhoxalApiMarker;
                            const NAME: &'static str = #response_name;
                            const VERSION: &'static str = #tree_id;
                            const CONTRACT: &'static str = #response_contract;
                            const TOPIC: &'static str = #key;
                            const ROLE: ::phoxal_bus::TopicRole = #role;
                            const DELIVERY: ::phoxal_bus::DeliveryFamily = #delivery;
                        }
                    });
                }
            }
        }

        // Child node modules, one level deeper.
        let mut child_mods = TokenStream::new();
        for child in &self.children {
            child_mods.extend(child.expand_module(tree_id, &family_path, &node_key_prefix));
        }

        quote! {
            pub mod #name {
                //! Version-local bodies for the `#name_str` node.

                // Forward the tree root's `Api` marker down exactly one hop from
                // this node's own parent (the tree module for a top-level node, or
                // the parent node module for a nested one). Every node module - at any
                // depth - carries this same single-hop re-export, so `Api` is always
                // reachable as `self::__PhoxalApiMarker` without computing how deep
                // this node sits.
                #[doc(hidden)]
                pub use super::__PhoxalApiMarker;

                #types
                #impls
                #child_mods
            }
        }
    }
}

/// Make inherited fields public so participant code in other crates can construct
/// and read ordinary wire bodies directly. An explicitly narrower visibility is
/// preserved for bodies whose constructors/accessors own an invariant (for
/// example, finite control targets).
fn with_pub_fields(mut item: ItemStruct) -> ItemStruct {
    if let syn::Fields::Named(named) = &mut item.fields {
        for field in &mut named.named {
            if matches!(field.vis, syn::Visibility::Inherited) {
                field.vis = syn::Visibility::Public(syn::token::Pub::default());
            }
        }
    }
    item
}
