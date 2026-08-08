//! The api-local `topic` builder module: the same node/leaf tree emitted
//! twice, once per side, so taking the wrong side of a topic does not compile.
//!
//! Both trees walk identical node methods and format identical keys; only the
//! brand a leaf method returns differs. Keys are built from the same node path
//! the [`super::bodies`] pass uses for `ContractBody::TOPIC`, so the builder
//! and the const cannot drift apart.

use proc_macro2::TokenStream;
use quote::quote;
use syn::Ident;

use super::model::{MaterializedTree, Node, TopicDef, TopicKind, TopicLeaf};

/// Which side a generated builder tree brands its leaves with.
///
/// - [`Side::Client`] - the PUBLIC `topic::client()...` tree. A `command` leaf
///   yields `Topic<Publish<B>>` (the client sends commands), a `state` leaf
///   yields `Topic<Subscribe<B>>` (the client observes state), and a `query`
///   leaf yields `Topic<AskQuery<Req, Resp>>` (the client calls).
/// - [`Side::Owner`] - the `topic::owner()...` tree. The brands flip:
///   `command` -> `Subscribe` (the owner reads its control input), `state` ->
///   `Publish` (the owner emits telemetry), `query` -> `ServeQuery` (the owner
///   serves).
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
    /// Self-contained absolute paths: a builder leaf needs to name a body type
    /// that lives in the PARALLEL type-tree hanging off the same tree module
    /// (`topic::component::motor::Builder` needs `component::motor::Command`).
    /// Rather than counting `super::` hops back to the tree root and down again
    /// per leaf, `topic` seeds one hidden alias per top-level node
    /// (`#[doc(hidden)] pub use super::component as __phoxal_type_root_component;`,
    /// a single, always-valid hop since `topic` is a direct child of the tree
    /// module) and `owner` re-forwards each of them one hop further. Every
    /// builder module under either side then imports its own top-level node's
    /// alias - a single hop from its immediate parent - under the uniform local
    /// name `__phoxal_type_root`, and deeper builder modules just forward THAT
    /// one hop at a time. A leaf reference is then always
    /// `self::__phoxal_type_root::…::Body`: no supers count, no dependency on
    /// how deep the node was authored.
    pub(super) fn expand_topic_module(&self) -> TokenStream {
        let mut client_root_methods = TokenStream::new();
        let mut client_builder_mods = TokenStream::new();
        let mut owner_root_methods = TokenStream::new();
        let mut owner_builder_mods = TokenStream::new();
        let mut type_root_seeds = TokenStream::new();
        let mut type_root_forwards = TokenStream::new();
        for node in &self.nodes {
            let name = &node.name;
            let alias = type_root_alias_ident(name);

            client_root_methods.extend(node.entry_method());
            client_builder_mods.extend(node.expand_builder_module(&self.id, &[], Side::Client));
            owner_root_methods.extend(node.entry_method());
            owner_builder_mods.extend(node.expand_builder_module(&self.id, &[], Side::Owner));

            type_root_seeds.extend(quote! {
                #[doc(hidden)]
                pub use super::#name as #alias;
            });
            type_root_forwards.extend(quote! {
                #[doc(hidden)]
                pub use super::#alias;
            });
        }

        quote! {
            /// Api-local topic builders, side-branded. The PUBLIC
            /// `topic::client()...` chain is the CLIENT side; the OWNER side is
            /// the equally explicit [`topic::owner()`](owner). Every leaf binds the
            /// topic's node-path/kind to a tree-local body and the side it grants.
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

                // Per-top-level-node type-tree aliases (self-contained absolute
                // paths): seeded here because `topic` is always exactly one hop
                // from the tree module that holds the type tree.
                #type_root_seeds

                #client_builder_mods

                /// Begin an OWNER topic path for this tree.
                pub fn owner() -> owner::Root {
                    owner::Root
                }

                /// Owner-side topic builders. A participant acquires topics of its OWN
                /// node here, getting the publish/subscribe/serve side it must take.
                /// Consumed topics still go through [`client()`](self::client).
                pub mod owner {
                    /// Root of the owner topic builder chain. `#[non_exhaustive]` keeps
                    /// [`owner()`](super::owner) as its sole public entry point.
                    #[non_exhaustive]
                    pub struct Root;
                    impl Root {
                        #owner_root_methods
                    }

                    // Forward each top-level node's type-tree alias one more hop, from
                    // `topic` into `owner` (still a single, always-valid hop).
                    #type_root_forwards

                    #owner_builder_mods
                }
            }
        }
    }
}

impl Node {
    /// The method on a parent builder (or `Root`) that enters this node's
    /// builder. A static node takes no args; a dynamic node takes its var as
    /// `impl Display`. The returned builder carries all vars bound so far plus
    /// this node's (if any).
    fn entry_method(&self) -> TokenStream {
        let name = &self.name;
        let name_str = name.to_string();
        let target = quote!(#name::Builder);
        match &self.var {
            Some(var) => quote! {
                #[doc = #name_str]
                pub fn #name(self, #var: impl ::core::fmt::Display) -> #target {
                    #name::Builder::__from(self, #var.to_string())
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
    /// in-scope var as a `String`; leaf methods format the key from those fields
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
            .map(|f| quote! { pub(super) #f: String })
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
                    pub(super) fn __from(#parent_pat: #parent_builder_ty, #var: String) -> Self {
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

        // Self-contained absolute path to this top-level node's type-tree: at the
        // top of a top-level node's builder subtree (`ancestors` empty) import the
        // alias `expand_topic_module` seeded one hop up (in `topic` for the client
        // side, in `topic::owner` for the owner side - both are that alias's direct
        // parent). Every deeper builder module just re-forwards it one more hop under
        // the same local name, so a leaf at any depth reaches its body type through
        // `self::__phoxal_type_root` with no supers count.
        let type_root_import = if ancestors.is_empty() {
            let alias = type_root_alias_ident(name);
            quote! {
                #[doc(hidden)]
                pub use super::#alias as __phoxal_type_root;
            }
        } else {
            quote! {
                #[doc(hidden)]
                pub use super::__phoxal_type_root;
            }
        };

        let builder_doc = format!("Topic builder for the `{name_str}` node.");
        quote! {
            pub mod #name {
                #type_root_import

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
    /// The body path is built from `self::__phoxal_type_root` (this top-level
    /// node's type-tree alias, forwarded one hop at a time down from
    /// `topic`/`topic::owner`), followed by the node path's segments after the
    /// top-level one (which the alias already denotes), then the body ident.
    /// This is a fixed-shape reference that never depends on how deep the node
    /// was authored or on which side is being built.
    ///
    /// The brand is picked from `(role, side)`:
    ///
    /// - `command`: client publishes (`Publish`), owner subscribes (`Subscribe`).
    /// - `state` / `measurement` / `diagnostic`: client subscribes (`Subscribe`),
    ///   owner publishes (`Publish`).
    /// - `query`: client asks (`AskQuery`), owner serves (`ServeQuery`).
    fn builder_leaf_kind(&self, path: &[NodeSeg], side: Side) -> TokenStream {
        // `path[0]` is the top-level node - exactly what `__phoxal_type_root` already
        // aliases - so only the segments AFTER it need to be descended.
        let rest_path: Vec<&Ident> = path.iter().skip(1).map(|s| &s.name).collect();
        let body_path =
            |body: &Ident| quote! { self::__phoxal_type_root #(::#rest_path)* :: #body };
        match &self.kind {
            TopicKind::PubSub(body) => {
                let b = body_path(body);
                // Every pub/sub role shares the wire shape and differs only in
                // which side publishes; the role + side pick the brand. (A `query`
                // role never carries a `PubSub` kind - the parser pairs it with
                // `TopicKind::Query` - and `owner_publishes` treats it like an
                // owner-published role, which is unreachable but harmless.)
                let owner_publishes = self.role.owner_publishes();
                match side {
                    Side::Owner if owner_publishes => quote! { ::phoxal_bus::Publish<#b> },
                    Side::Client if !owner_publishes => quote! { ::phoxal_bus::Publish<#b> },
                    _ => quote! { ::phoxal_bus::Subscribe<#b> },
                }
            }
            TopicKind::Query { request, response } => {
                let req = body_path(request);
                let resp = body_path(response);
                match side {
                    Side::Client => quote! { ::phoxal_bus::AskQuery<#req, #resp> },
                    Side::Owner => quote! { ::phoxal_bus::ServeQuery<#req, #resp> },
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
    /// with `ContractBody::TOPIC`.
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

/// The name of the hidden alias, seeded once in `topic` per top-level node, that
/// re-exports that node's type-tree module (e.g. `component`) under a name that
/// cannot collide with the SAME-named builder submodule `topic` also declares for
/// it. Builder modules import it (and re-forward it downward) as
/// `__phoxal_type_root`.
fn type_root_alias_ident(node_name: &Ident) -> Ident {
    quote::format_ident!("__phoxal_type_root_{}", node_name)
}
