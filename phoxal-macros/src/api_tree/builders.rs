//! The api-local `topic` builder module: the same node/leaf tree emitted
//! twice, once per side, so taking the wrong side of a topic does not compile.
//!
//! Both trees walk identical node methods and format identical keys; only the
//! brand a leaf method returns differs. Keys are built from the same node path
//! the [`super::bodies`] pass uses for descriptor `TOPIC`, so they cannot drift.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Ident;

use super::model::{MaterializedTree, Node, TopicDef, TopicKind, TopicLeaf};

/// Which side a generated builder tree brands its leaves with.
///
/// - [`Side::Client`] - the PUBLIC `topic::client()...` tree. A setpoint or
///   stream leaf yields `Topic<Publish<E>>` (the client sends intent/chunks),
///   a state/sample/event leaf yields `Topic<Subscribe<E>>` (the client observes
///   products), and a query
///   leaf yields `Topic<AskQuery<E>>` (the client calls).
/// - [`Side::Owner`] - the `topic::owner()...` tree. The brands flip:
///   setpoint/stream -> `Subscribe` (the owner reads control/chunks),
///   state/sample/event -> `Publish` (the owner emits products),
///   `query` -> `ServeQuery`
///   (the owner serves through `ServeQuery<E>`).
#[derive(Clone, Copy)]
enum Side {
    Client,
    Owner,
}

/// One node along a builder path: its literal name (a key segment) and, if the
/// node is dynamic, the variable field it binds. The path is enough to build both
/// the carried-field set and the `format!` key for a leaf.
#[derive(Clone)]
struct NodeSeg {
    name: Ident,
    var: Option<Ident>,
}

impl MaterializedTree {
    /// Emit the api-local `topic` builder module with BOTH side trees.
    ///
    /// The PUBLIC client tree lives directly under `topic` (`topic::client()` +
    /// a builder module per node); the OWNER tree lives under `topic::owner`
    /// (`topic::owner()` + the same builder modules, one level deeper).
    ///
    /// Builder leaves name generated endpoint descriptors through one hidden
    /// root alias. It is forwarded one hop at every nesting level, so endpoint
    /// references never depend on a counted `super::` path.
    pub(super) fn expand_topic_module(&self) -> TokenStream {
        let mut client_root_methods = TokenStream::new();
        let mut client_builder_mods = TokenStream::new();
        let mut owner_root_methods = TokenStream::new();
        let mut owner_builder_mods = TokenStream::new();
        for node in &self.nodes {
            client_root_methods.extend(node.entry_method());
            client_builder_mods.extend(node.expand_builder_module(&self.id, &[], Side::Client));
            owner_root_methods.extend(node.entry_method());
            owner_builder_mods.extend(node.expand_builder_module(&self.id, &[], Side::Owner));
        }

        quote! {
            /// Api-local topic builders, side-branded. The PUBLIC
            /// `topic::client()...` chain is the CLIENT side; the OWNER side is
            /// the equally explicit [`topic::owner()`](owner). Every leaf binds the
            /// topic's node path and descriptor to the side it grants.
            pub mod topic {
                /// Begin a CLIENT topic path for this tree.
                pub fn client() -> Root {
                    Root
                }

                /// Root of the client topic builder chain. `#[non_exhaustive]` keeps
                /// [`client()`](self::client) as its sole public entry point.
                #[non_exhaustive]
                pub struct Root;
                impl Root {
                    #client_root_methods
                }

                #[doc(hidden)]
                pub use super::endpoint as __phoxal_endpoint_root;

                #client_builder_mods

                /// Begin an OWNER topic path for this tree.
                pub fn owner() -> owner::Root {
                    owner::Root
                }

                /// Owner-side topic builders. A participant acquires topics of its OWN
                /// node here, getting the publish/subscribe/serve side it must take.
                /// Consumed topics still go through [`client()`](self::client).
                pub mod owner {
                    #[doc(hidden)]
                    pub use super::__phoxal_endpoint_root;

                    /// Root of the owner topic builder chain. `#[non_exhaustive]` keeps
                    /// [`owner()`](super::owner) as its sole public entry point.
                    #[non_exhaustive]
                    pub struct Root;
                    impl Root {
                        #owner_root_methods
                    }

                    #owner_builder_mods
                }
            }
        }
    }
}

impl Node {
    /// The method on a parent builder (or `Root`) that enters this node's
    /// builder. A static node takes no args; a dynamic node takes its var as
    /// `impl Display` and validates it as one concrete [`KeySegment`]. The
    /// returned builder carries all vars bound so far plus this node's (if any).
    fn entry_method(&self) -> TokenStream {
        let name = &self.name;
        let name_str = name.to_string();
        let target = quote!(#name::Builder);
        match &self.var {
            Some(var) => quote! {
                #[doc = #name_str]
                pub fn #name(self, #var: impl ::core::fmt::Display) -> ::core::result::Result<#target, ::phoxal_bus::KeySegmentError> {
                    let #var = ::phoxal_bus::KeySegment::new(#var.to_string())?;
                    Ok(#name::Builder::__from(self, #var))
                }
            },
            None => quote! {
                #[doc = #name_str]
                pub fn #name(self) -> #target {
                    #name::Builder::__from(self)
                }
            },
        }
    }

    /// Emit the builder module for this node (and recursively its children) on
    /// `side`. `ancestors` is the chain of nodes from the tree root down to (but
    /// excluding) this node, in order. Each builder is a struct that stores every
    /// in-scope var as a validated `KeySegment`; leaf methods format the key from those fields
    /// and brand the returned `Topic` per `side` (the same structure and keys on
    /// both sides; only the leaf brand differs).
    fn expand_builder_module(
        &self,
        tree_id: &str,
        ancestors: &[NodeSeg],
        side: Side,
    ) -> TokenStream {
        let name = &self.name;
        let name_str = name.to_string();

        // Full node path (root -> node) and the variables in scope (ancestors' +
        // this node's, in order).
        let mut path: Vec<NodeSeg> = ancestors.to_vec();
        path.push(NodeSeg {
            name: name.clone(),
            var: self.var.clone(),
        });
        let vars: Vec<&Ident> = path.iter().filter_map(|s| s.var.as_ref()).collect();
        let ancestor_vars: Vec<&Ident> = ancestors.iter().filter_map(|s| s.var.as_ref()).collect();

        // Builder fields use positional storage names (`__seg0`, `__seg1`, …), one per
        // in-scope var, so a var name reused across the path (e.g. `a(id) { b(id) { … } }`)
        // never collides into duplicate struct fields. The original var ident stays the
        // public method param + doc placeholder; only the private storage is positional.
        let field_idents: Vec<Ident> = (0..vars.len()).map(seg_field).collect();
        let ancestor_field_idents: Vec<Ident> = (0..ancestor_vars.len()).map(seg_field).collect();
        let field_decls: Vec<TokenStream> = field_idents
            .iter()
            .map(|f| quote! { pub(super) #f: ::phoxal_bus::KeySegment })
            .collect();

        // The `__from` constructor: a static node takes only the parent builder; a
        // dynamic node also takes its var's string. It assembles all carried fields by
        // moving the parent's fields and adding this node's. From inside this node's
        // builder module, the parent builder is `super::Root` for a top-level node
        // (`ancestors` empty) and `super::Builder` for a nested one. Unlike the
        // type-tree cross-reference below, this names this builder's own immediate
        // parent module, which is by construction always exactly one hop away
        // regardless of depth, so it never needed the alias-forwarding scheme.
        let parent_builder_ty = if ancestors.is_empty() {
            quote! { super::Root }
        } else {
            quote! { super::Builder }
        };
        let parent_fields: Vec<TokenStream> = ancestor_field_idents
            .iter()
            .map(|f| quote! { #f: __parent.#f })
            .collect();
        // `Root` is a unit struct with no fields; suppress the unused-parent lint when
        // this builder carries nothing from the parent.
        let parent_pat = if ancestor_vars.is_empty() {
            quote! { _parent }
        } else {
            quote! { __parent }
        };
        let ctor = match &self.var {
            Some(var) => {
                // This node's var is the next positional field after the ancestors'.
                let new_field = seg_field(ancestor_vars.len());
                quote! {
                    pub(super) fn __from(#parent_pat: #parent_builder_ty, #var: ::phoxal_bus::KeySegment) -> Self {
                        Self { #(#parent_fields,)* #new_field: #var }
                    }
                }
            }
            None => quote! {
                pub(super) fn __from(#parent_pat: #parent_builder_ty) -> Self {
                    Self { #(#parent_fields,)* }
                }
            },
        };

        // Leaf methods: each topic leaf returns a typed `Topic`. A node with no vars in
        // scope (fully static path) uses `Topic::new_static` over a literal key;
        // otherwise `new_owned` with a `format!` filling the carried vars.
        let mut leaf_methods = TokenStream::new();
        for topic in &self.topics {
            let leaf = topic.leaf.method_ident();
            let kind_ty = topic.builder_leaf_kind(&path, side);
            let (fmt_str, doc_key) = topic.leaf.builder_key_parts(tree_id, &path);
            let constructor = if field_idents.is_empty() {
                quote! { ::phoxal_bus::Topic::new_static(#fmt_str) }
            } else {
                quote! {
                    ::phoxal_bus::Topic::new_owned(::std::format!(#fmt_str, #(self.#field_idents),*))
                }
            };
            leaf_methods.extend(quote! {
                #[doc = #doc_key]
                pub fn #leaf(self) -> ::phoxal_bus::Topic<#kind_ty> {
                    #constructor
                }
            });
        }

        // Methods entering child builders, and the child builder modules themselves.
        let mut child_methods = TokenStream::new();
        let mut child_mods = TokenStream::new();
        for child in &self.children {
            child_methods.extend(child.entry_method());
            child_mods.extend(child.expand_builder_module(tree_id, &path, side));
        }

        let endpoint_root_import = quote! {
            #[doc(hidden)]
            pub use super::__phoxal_endpoint_root;
        };

        let builder_doc = format!("Topic builder for the `{name_str}` node.");
        quote! {
            pub mod #name {
                #endpoint_root_import

                #[doc = #builder_doc]
                // Keep a path builder reachable only through its parent builder, so
                // `client()` and `owner()` remain the explicit entry points.
                #[non_exhaustive]
                pub struct Builder {
                    #(#field_decls,)*
                }
                impl Builder {
                    #ctor
                    #leaf_methods
                    #child_methods
                }

                #child_mods
            }
        }
    }
}

impl TopicDef {
    /// The branded `Kind` type for this leaf on `side`.
    ///
    /// The brand is picked from endpoint direction and builder side.
    fn builder_leaf_kind(&self, path: &[NodeSeg], side: Side) -> TokenStream {
        let endpoint_path: Vec<&Ident> = path.iter().map(|s| &s.name).collect();
        match &self.kind {
            TopicKind::PubSub(_) => {
                let endpoint = self.endpoint_ident();
                let endpoint =
                    quote! { self::__phoxal_endpoint_root #(::#endpoint_path)* :: #endpoint };
                let owner_publishes = self.owner_publishes;
                match side {
                    Side::Owner if owner_publishes => quote! { ::phoxal_bus::Publish<#endpoint> },
                    Side::Client if !owner_publishes => quote! { ::phoxal_bus::Publish<#endpoint> },
                    _ => quote! { ::phoxal_bus::Subscribe<#endpoint> },
                }
            }
            TopicKind::Query { .. } => {
                let endpoint = self.endpoint_ident();
                let endpoint =
                    quote! { self::__phoxal_endpoint_root #(::#endpoint_path)* :: #endpoint };
                let req = endpoint.clone();
                match side {
                    Side::Client => quote! { ::phoxal_bus::AskQuery<#req> },
                    Side::Owner => quote! { ::phoxal_bus::ServeQuery<#req> },
                }
            }
        }
    }
}

impl TopicLeaf {
    /// Build this leaf's key in two forms: the `format!` template (the tree
    /// identity as a literal leading segment, then literal node-name segments,
    /// `{}` for each dynamic var, optionally then `/leaf`) and the
    /// human-readable `{var}`-placeholder doc key. Both are derived from the
    /// node path so the concrete key and the documented key stay in lockstep
    /// with descriptor `TOPIC`.
    fn builder_key_parts(&self, tree_id: &str, path: &[NodeSeg]) -> (String, String) {
        let mut fmt_segs = vec![tree_id.to_string()];
        let mut doc_segs = vec![tree_id.to_string()];
        for seg in path {
            let name = seg.name.to_string();
            match &seg.var {
                Some(var) => {
                    fmt_segs.push(format!("{name}/{{}}"));
                    doc_segs.push(format!("{name}/{{{var}}}"));
                }
                None => {
                    fmt_segs.push(name.clone());
                    doc_segs.push(name);
                }
            }
        }
        match self {
            TopicLeaf::Named(leaf) => {
                let leaf = leaf.to_string();
                (
                    format!("{}/{}", fmt_segs.join("/"), leaf),
                    format!("{}/{}", doc_segs.join("/"), leaf),
                )
            }
            TopicLeaf::Node => (fmt_segs.join("/"), doc_segs.join("/")),
        }
    }
}

/// Positional private storage field for the `i`-th in-scope dynamic var of a
/// builder (`__seg0`, `__seg1`, …). Positional (not the var ident) so a var name
/// reused across a nested path never produces duplicate builder struct fields.
fn seg_field(i: usize) -> Ident {
    quote::format_ident!("__seg{}", i)
}
