//! `phoxal_api_tree!` — the single API layer (D60/D61).
//!
//! Grammar. The body of a `version` is a tree of **nodes**. A node is either
//! static (`name { … }`) or dynamic (`name(var) { … }`); it can be nested to any
//! depth and may hold any mix of types (`struct`/`enum`), `topic` declarations,
//! and child nodes. Every topic declares a **role**: `topic <leaf>: command
//! <Body>;` (a control input the owner subscribes), `topic <leaf>: state <Body>;`
//! (telemetry the owner publishes), or `topic <leaf>: query <Req> => <Resp>;`
//! (request/response). `command` and `state` are both pub/sub on the wire; the
//! role drives the side-branded builders (L1): the public client builder
//! (`api::topic::new()...`) and the owner builder (`api::topic::internal::new(cap)...`)
//! return side-branded topics (`Publish`/`Subscribe`/`AskQuery`/`ServeQuery`), so
//! taking the wrong side does not compile. The owner entry takes the runner-minted
//! `OwnerCap` (L2). The role is also emitted as a `ROLE`
//! const on each body (D63). A topic's key and dynamism are
//! derived from the node path, not from per-topic params; a topic whose node path
//! contains at least one `(var)` node is dynamic, one with none is static.
//! `extends` API inheritance is supported (and recurses the node tree).
//!
//! ```text
//! phoxal_api_tree! {
//!     version y2026_1 {
//!         drive {                                  // static node
//!             struct Target { linear_x_mps: f32, angular_z_radps: f32 }
//!             topic target: command Target;        // key drive/target, FAMILY drive::Target
//!             struct State { /* … */ }
//!             topic state: state State;            // owner-published telemetry
//!         }
//!         component(instance) {                    // literal "component" + var {instance}
//!             motor(capability) {                  // literal "motor" + var {capability}
//!                 enum Command { Velocity(f32), Torque(f32), Stop }
//!                 topic command: command Command;
//!                 // path   api::y2026_1::component::motor::Command
//!                 // key    component/{instance}/motor/{capability}/command
//!                 // FAMILY component::motor::Command
//!             }
//!         }
//!     }
//!     version y2026_2 extends y2026_1 {
//!         // inherits every y2026_1 node/type; overrides drive::Target and adds a
//!         // new `battery` node — inherited types are re-emitted fresh.
//!         drive { struct Target { linear_x_mps: f32, angular_z_radps: f32, curvature: Option<f32> }
//!                 topic target: command Target; }
//!         battery { struct State { soc: f32 } topic state: state State; }
//!     }
//! }
//! ```
//!
//! Each `version` becomes a `pub mod y2026_N` carrying a marker `enum Api {}`
//! (`ApiVersion`), a nested `pub mod` per node holding that node's version-local
//! bodies (plain serde types, no `{"v":…}` wrapper — D62) and their
//! `ContractBody` impls, plus an api-local `topic` builder module. A
//! `version … extends P` inherits `P`'s effective node tree: a child node merges
//! into the parent node of the same name along the path (child types/topics
//! override by name, a new child subtree is appended), and every inherited type is
//! re-emitted *fresh* under the child version — same `FAMILY`/`TOPIC`, a different
//! `Api` (D61).
//!
//! Wire identity is **per-type, by transitive shape**: an inherited type re-emits
//! the parent's definition verbatim, so a type that does **not** transitively
//! reference an overridden type is byte-identical to its parent (e.g. a flat
//! `localize::State`). A type that *contains* an overridden type resolves that
//! name to the child's overridden version — e.g. if the child overrides
//! `drive::Target`, the inherited `drive::State { target: Target, … }` embeds the
//! **new** `Target` and is therefore *not* identical to the parent's `State`.
//! That is correct versioning: a contract changes with its dependencies, and the
//! whole change is coherent because one graph runs one API version (D59).

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, ItemEnum, ItemStruct, Token};

use crate::util::body_derives;

mod kw {
    syn::custom_keyword!(version);
    syn::custom_keyword!(extends);
    syn::custom_keyword!(topic);
    syn::custom_keyword!(command);
    syn::custom_keyword!(state);
    syn::custom_keyword!(query);
}

pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let tree: ApiTree = syn::parse2(input)?;
    tree.expand()
}

struct ApiTree {
    versions: Vec<Version>,
}

struct Version {
    name: Ident,
    extends: Option<Ident>,
    nodes: Vec<Node>,
}

/// One node in the api tree: a `name { … }` (static) or `name(var) { … }`
/// (dynamic) block that may hold types, topics, and nested child nodes.
#[derive(Clone)]
struct Node {
    name: Ident,
    /// The dynamic variable bound by this node (`None` for a static node). When
    /// present, the node contributes `name/{var}` to keys and a var-taking builder
    /// method.
    var: Option<Ident>,
    types: Vec<TypeDef>,
    topics: Vec<TopicDef>,
    children: Vec<Node>,
}

#[derive(Clone)]
enum TypeDef {
    Struct(ItemStruct),
    Enum(ItemEnum),
}

impl TypeDef {
    /// The declared name of the type (for inheritance overlay by name).
    fn ident(&self) -> &Ident {
        match self {
            TypeDef::Struct(item) => &item.ident,
            TypeDef::Enum(item) => &item.ident,
        }
    }
}

#[derive(Clone)]
struct TopicDef {
    leaf: Ident,
    kind: TopicKind,
    /// The semantic role declared by the topic's role keyword (`command` /
    /// `state` / `query`). `command` and `state` both produce a [`TopicKind::PubSub`]
    /// on the wire, while `query` produces a [`TopicKind::Query`]. The role selects
    /// the SIDE BRAND in the generated builders (L1): per (role, side) a leaf
    /// returns `Publish` / `Subscribe` / `AskQuery` / `ServeQuery`, so the public
    /// (client) and `internal` (owner) builders return different branded topics.
    /// The role is also emitted as a `ROLE` const on each body; it is not yet
    /// surfaced by `emit-apis` (a later increment of plan #00).
    role: TopicRole,
}

/// The semantic role of a topic, mirroring `phoxal_bus::TopicRole`. Parsed from
/// the role keyword and threaded into the generated `ROLE` const.
#[derive(Clone, Copy)]
enum TopicRole {
    Command,
    State,
    Query,
}

impl TopicRole {
    /// The `phoxal_bus::TopicRole` variant path this role maps to.
    fn bus_variant(self) -> TokenStream {
        match self {
            TopicRole::Command => quote! { ::phoxal_bus::TopicRole::Command },
            TopicRole::State => quote! { ::phoxal_bus::TopicRole::State },
            TopicRole::Query => quote! { ::phoxal_bus::TopicRole::Query },
        }
    }
}

#[derive(Clone)]
enum TopicKind {
    PubSub(Ident),
    Query { request: Ident, response: Ident },
}

impl Parse for ApiTree {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let mut versions = Vec::new();
        while !input.is_empty() {
            versions.push(input.parse()?);
        }
        if versions.is_empty() {
            return Err(input.error("phoxal_api_tree! requires at least one `version` block"));
        }
        Ok(ApiTree { versions })
    }
}

impl Parse for Version {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::version>()?;
        let name: Ident = input.parse()?;
        let extends = if input.peek(kw::extends) {
            input.parse::<kw::extends>()?;
            Some(input.parse::<Ident>()?)
        } else {
            None
        };
        let body;
        syn::braced!(body in input);
        let mut nodes = Vec::new();
        while !body.is_empty() {
            nodes.push(body.parse()?);
        }
        Ok(Version {
            name,
            extends,
            nodes,
        })
    }
}

impl Parse for Node {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let name: Ident = input.parse()?;
        // Optional `(var)` makes the node dynamic.
        let var = if input.peek(syn::token::Paren) {
            let content;
            syn::parenthesized!(content in input);
            let var: Ident = content.parse()?;
            if !content.is_empty() {
                return Err(content.error(
                    "a dynamic node binds exactly one variable, e.g. `motor(capability) { … }`",
                ));
            }
            Some(var)
        } else {
            None
        };

        let body;
        syn::braced!(body in input);
        let mut types = Vec::new();
        let mut topics = Vec::new();
        let mut children = Vec::new();
        while !body.is_empty() {
            // Leading doc-comments / attributes apply to the next item; `topic`
            // declarations take none.
            let attrs = body.call(syn::Attribute::parse_outer)?;
            if body.peek(kw::topic) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a `topic` declaration",
                    ));
                }
                topics.push(body.parse()?);
            } else if body.peek(Token![struct]) {
                let mut item: ItemStruct = body.parse()?;
                item.attrs = attrs;
                item.vis = syn::Visibility::Public(syn::token::Pub::default());
                types.push(TypeDef::Struct(item));
            } else if body.peek(Token![enum]) {
                let mut item: ItemEnum = body.parse()?;
                item.attrs = attrs;
                item.vis = syn::Visibility::Public(syn::token::Pub::default());
                types.push(TypeDef::Enum(item));
            } else if body.peek(Ident)
                && (body.peek2(syn::token::Paren) || body.peek2(syn::token::Brace))
            {
                // `name(var) { … }` or `name { … }` — a child node.
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a child node declaration",
                    ));
                }
                children.push(body.parse()?);
            } else {
                return Err(body.error(
                    "expected `struct`, `enum`, `topic …;`, or a child node `name { … }` / \
                     `name(var) { … }` inside an API node block",
                ));
            }
        }
        Ok(Node {
            name,
            var,
            types,
            topics,
            children,
        })
    }
}

impl Parse for TopicDef {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::topic>()?;
        let leaf: Ident = input.parse()?;
        input.parse::<Token![:]>()?;
        // Every topic declares a role. `command`/`state` carry a single pub/sub
        // body and differ by role; `query` carries request/response. The role
        // rides alongside the kind and selects the side brand in the generated
        // builders (L1): a `command` leaf is `Publish` on the public builder and
        // `Subscribe` on `internal`; a `state` leaf is the reverse.
        let (kind, role) = if input.peek(kw::command) {
            input.parse::<kw::command>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::Command)
        } else if input.peek(kw::state) {
            input.parse::<kw::state>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::State)
        } else if input.peek(kw::query) {
            input.parse::<kw::query>()?;
            let request: Ident = input.parse()?;
            input.parse::<Token![=>]>()?;
            let response: Ident = input.parse()?;
            (TopicKind::Query { request, response }, TopicRole::Query)
        } else {
            return Err(input.error(
                "expected a topic role: `command <Body>`, `state <Body>`, or \
                 `query <Req> => <Resp>`",
            ));
        };
        input.parse::<Token![;]>()?;
        Ok(TopicDef { leaf, kind, role })
    }
}

impl ApiTree {
    fn expand(&self) -> syn::Result<TokenStream> {
        let mut out = TokenStream::new();
        // Resolve each version's effective node tree (inheriting from `extends`)
        // and generate from the resolved set. Inherited types are re-emitted
        // *fresh* under the child version — same `FAMILY`/`TOPIC`, a different
        // `Api` marker — so unchanged contracts are wire-identical by construction
        // while the compile-time `Api` bound stays sound (D61).
        let mut resolved: std::collections::HashMap<String, Vec<Node>> =
            std::collections::HashMap::new();

        for version in &self.versions {
            let effective = match &version.extends {
                None => version.nodes.clone(),
                Some(parent) => {
                    let base = resolved.get(&parent.to_string()).ok_or_else(|| {
                        syn::Error::new_spanned(
                            parent,
                            format!(
                                "`extends {parent}`: unknown API version (it must be declared \
                                 earlier in the same phoxal_api_tree! invocation)"
                            ),
                        )
                    })?;
                    overlay_nodes(base, &version.nodes)?
                }
            };
            resolved.insert(version.name.to_string(), effective.clone());
            out.extend(expand_version(&version.name, &effective)?);
        }
        Ok(out)
    }
}

/// Overlay a child version's declared nodes onto the parent's effective tree,
/// recursively. A child node merges into the parent node of the same name: child
/// types override parent types by ident, child topics override by leaf, and child
/// subtrees recurse the same overlay. A wholly-new child node (or subtree) is
/// appended (D61).
fn overlay_nodes(base: &[Node], child: &[Node]) -> syn::Result<Vec<Node>> {
    let mut result: Vec<Node> = base.to_vec();
    for cn in child {
        if let Some(bn) = result.iter_mut().find(|n| n.name == cn.name) {
            // A same-named node must keep the parent's dynamic-ness: flipping
            // whether it binds a `(var)` would silently rewrite every descendant
            // topic key across versions. Reject it (fail-closed) rather than
            // overwrite — an inherited node keeps the same `(var)` (or absence).
            if bn.var.as_ref().map(Ident::to_string) != cn.var.as_ref().map(Ident::to_string) {
                return Err(syn::Error::new_spanned(
                    &cn.name,
                    format!(
                        "`extends`: node `{}` redeclares its dynamic variable differently from \
                         the inherited version; a same-named node must keep the same `(var)` (or \
                         its absence) so inherited topic keys stay stable",
                        cn.name
                    ),
                ));
            }
            for ct in &cn.types {
                if let Some(slot) = bn.types.iter_mut().find(|t| t.ident() == ct.ident()) {
                    *slot = ct.clone();
                } else {
                    bn.types.push(ct.clone());
                }
            }
            for ctp in &cn.topics {
                if let Some(slot) = bn.topics.iter_mut().find(|t| t.leaf == ctp.leaf) {
                    *slot = ctp.clone();
                } else {
                    bn.topics.push(ctp.clone());
                }
            }
            bn.children = overlay_nodes(&bn.children, &cn.children)?;
        } else {
            result.push(cn.clone());
        }
    }
    Ok(result)
}

fn expand_version(name: &Ident, nodes: &[Node]) -> syn::Result<TokenStream> {
    let mod_name = name;
    let id = name.to_string();

    // Node modules (types + ContractBody impls), recursive. Each top-level node is
    // at depth 1 under the version. The family prefix (`::`-joined node names) and
    // the key prefix (`/`-joined `name` or `name/{var}` segments) are threaded
    // down the walk.
    let mut node_mods = TokenStream::new();
    for node in nodes {
        node_mods.extend(expand_node_module(node, 1, "", "")?);
    }

    let topic_mod = expand_topic_module(nodes)?;

    Ok(quote! {
        pub mod #mod_name {
            //! Dated API version `#id` — version-local wire bodies + topics.

            /// Zero-variant marker identifying this API version (D60).
            #[derive(Clone, Copy, Debug)]
            pub enum Api {}
            impl ::phoxal_bus::ApiVersion for Api {
                const ID: &'static str = #id;
            }

            #node_mods

            #topic_mod
        }
    })
}

/// Emit a `pub mod <name>` for a node at `depth` under the version (depth 1 = a
/// direct child of the version). The module carries the node's types, the
/// `ContractBody` impls for its topics, and — recursively — its child node
/// modules. Variables never appear in the module path (D61).
///
/// `family_prefix` is the `::`-joined ancestor node names (empty at the root);
/// `key_prefix` is the `/`-joined ancestor key segments (`name` or `name/{var}`,
/// empty at the root). The node appends its own contribution to each.
fn expand_node_module(
    node: &Node,
    depth: usize,
    family_prefix: &str,
    key_prefix: &str,
) -> syn::Result<TokenStream> {
    let name = &node.name;
    let name_str = name.to_string();
    let derives = body_derives();

    // This node's family path (`n1::n2::…::nk`) and key prefix (`…/name` or
    // `…/name/{var}`), vars excluded from the family path.
    let family_path = join_seg(family_prefix, "::", &name_str);
    let key_seg = match &node.var {
        Some(var) => format!("{}/{{{}}}", name_str, var),
        None => name_str.clone(),
    };
    let node_key_prefix = join_seg(key_prefix, "/", &key_seg);

    // `super` repeated `depth` times reaches the version module (which holds `Api`).
    let api_supers = supers(depth);

    let mut types = TokenStream::new();
    for ty in &node.types {
        match ty {
            TypeDef::Struct(item) => {
                let item = with_pub_fields_struct(item.clone());
                types.extend(quote! { #derives #item });
            }
            TypeDef::Enum(item) => {
                types.extend(quote! { #derives #item });
            }
        }
    }

    let mut impls = TokenStream::new();
    for topic in &node.topics {
        let key = format!("{}/{}", node_key_prefix, topic.leaf);
        let role = topic.role.bus_variant();
        match &topic.kind {
            TopicKind::PubSub(body) => {
                let family_const = format!("{}::{}", family_path, body);
                // The role rides as an inherent `#[doc(hidden)] pub const ROLE` on
                // the body: additive surface that does not touch `ContractBody`.
                // The side-branded builders are what enforce owner/client (L1);
                // the role is not yet emitted by `emit-apis` (a later increment of
                // plan #00).
                impls.extend(quote! {
                    impl ::phoxal_bus::ContractBody for #body {
                        type Api = #api_supers Api;
                        const FAMILY: &'static str = #family_const;
                        const TOPIC: &'static str = #key;
                    }
                    impl #body {
                        /// The topic role recorded by `phoxal_api_tree!` (D63).
                        #[doc(hidden)]
                        pub const ROLE: ::phoxal_bus::TopicRole = #role;
                    }
                });
            }
            TopicKind::Query { request, response } => {
                let req_family = format!("{}::{}", family_path, request);
                let resp_family = format!("{}::{}", family_path, response);
                impls.extend(quote! {
                    impl ::phoxal_bus::ContractBody for #request {
                        type Api = #api_supers Api;
                        const FAMILY: &'static str = #req_family;
                        const TOPIC: &'static str = #key;
                    }
                    impl ::phoxal_bus::ContractBody for #response {
                        type Api = #api_supers Api;
                        const FAMILY: &'static str = #resp_family;
                        const TOPIC: &'static str = #key;
                    }
                    impl #request {
                        /// The topic role recorded by `phoxal_api_tree!` (D63).
                        #[doc(hidden)]
                        pub const ROLE: ::phoxal_bus::TopicRole = #role;
                    }
                    impl #response {
                        /// The topic role recorded by `phoxal_api_tree!` (D63).
                        #[doc(hidden)]
                        pub const ROLE: ::phoxal_bus::TopicRole = #role;
                    }
                });
            }
        }
    }

    // Child node modules, one level deeper.
    let mut child_mods = TokenStream::new();
    for child in &node.children {
        child_mods.extend(expand_node_module(
            child,
            depth + 1,
            &family_path,
            &node_key_prefix,
        )?);
    }

    Ok(quote! {
        pub mod #name {
            //! Version-local bodies for the `#name_str` node.

            #types
            #impls
            #child_mods
        }
    })
}

/// `super::` repeated `n` times as a path prefix token stream (empty for
/// `n == 0`). Each `super` hop is `super ::`, so the result is usable as a path
/// prefix before a final ident (e.g. `super :: super :: Api`).
fn supers(n: usize) -> TokenStream {
    let mut ts = TokenStream::new();
    for _ in 0..n {
        ts.extend(quote! { super:: });
    }
    ts
}

/// Positional private storage field for the `i`-th in-scope dynamic var of a
/// builder (`__seg0`, `__seg1`, …). Positional (not the var ident) so a var name
/// reused across a nested path never produces duplicate builder struct fields.
fn seg_field(i: usize) -> Ident {
    quote::format_ident!("__seg{}", i)
}

/// Join two non-empty path/key segments with `sep`; if `prefix` is empty, return
/// `seg` alone.
fn join_seg(prefix: &str, sep: &str, seg: &str) -> String {
    if prefix.is_empty() {
        seg.to_string()
    } else {
        format!("{prefix}{sep}{seg}")
    }
}

/// Which side a generated builder tree brands its leaves with (L1, plan #00).
///
/// The api tree emits the builder tree TWICE over identical node/leaf structure
/// and keys, differing only by the brand each leaf carries:
///
/// - [`Side::Client`] - the PUBLIC `topic::new()...` tree. A `command` leaf yields
///   `Topic<Publish<B>>` (the client sends commands), a `state` leaf yields
///   `Topic<Subscribe<B>>` (the client observes state), and a `query` leaf yields
///   `Topic<AskQuery<Req, Resp>>` (the client calls).
/// - [`Side::Owner`] - the `topic::internal::new(cap)...` tree (a deliberate,
///   greppable, cap-gated owner opt-in; L2). The brands flip: `command` -> `Subscribe` (the owner
///   reads its control input), `state` -> `Publish` (the owner emits telemetry),
///   `query` -> `ServeQuery` (the owner serves).
#[derive(Clone, Copy)]
enum Side {
    Client,
    Owner,
}

/// Emit the api-local `topic` builder module with BOTH side trees (L1).
///
/// The PUBLIC client tree lives directly under `topic` (`topic::new()` + a builder
/// module per node); the OWNER tree lives under `topic::internal`
/// (`topic::internal::new(cap)` + the same builder modules, one level deeper). Both
/// mirror the node tree and format identical keys - only the leaf brand differs by
/// side. A dynamic node's method takes its variable as `impl Display` and carries
/// it forward; a leaf method formats the final key from the carried vars.
fn expand_topic_module(nodes: &[Node]) -> syn::Result<TokenStream> {
    let mut client_root_methods = TokenStream::new();
    let mut client_builder_mods = TokenStream::new();
    let mut owner_root_methods = TokenStream::new();
    let mut owner_builder_mods = TokenStream::new();
    for node in nodes {
        client_root_methods.extend(node_entry_method(node));
        client_builder_mods.extend(expand_builder_module(node, &[], Side::Client)?);
        owner_root_methods.extend(node_entry_method(node));
        owner_builder_mods.extend(expand_builder_module(node, &[], Side::Owner)?);
    }

    Ok(quote! {
        /// Api-local topic builders (D61), side-branded for L1 (plan #00). The
        /// PUBLIC `topic::new()...` chain is the CLIENT side; the OWNER side is the
        /// deliberate, greppable, capability-gated opt-in at
        /// [`topic::internal::new(cap)`](internal::new) (L2: it requires the
        /// runner-minted `OwnerCap`). Every leaf binds the topic's node-path/kind to
        /// a version-local body and the side it grants.
        pub mod topic {
            /// Begin a CLIENT topic path for this API version.
            pub fn new() -> Root {
                Root
            }

            /// Root of the client topic builder chain. `#[non_exhaustive]` so the
            /// only way to start a path is `topic::new()` (no direct `Root` literal).
            #[non_exhaustive]
            pub struct Root;
            impl Root {
                #client_root_methods
            }

            #client_builder_mods

            /// Owner-side topic builders (L1 + L2, plan #00). `internal::new(cap)...`
            /// is the deliberate, greppable owner opt-in: a runtime acquires the
            /// topics of its OWN node here, getting the publish/subscribe/serve side
            /// the owner must take (the inverse of the client brands). Consumed
            /// topics still go through the public [`new()`](self::new) chain.
            ///
            /// The entry requires the runner-minted `OwnerCap` (Layer 2): on the
            /// documented surface, owning a topic needs a capability obtained from
            /// `phoxal::SetupContext::owner_capability()`, so it cannot happen by
            /// accident.
            pub mod internal {
                /// Begin an OWNER topic path for this API version.
                ///
                /// Requires the runner-minted [`OwnerCap`](::phoxal_bus::OwnerCap)
                /// (Layer 2): pass `ctx.owner_capability()`. The cap is consumed
                /// only at this entry - it is not threaded through node methods or
                /// leaves.
                pub fn new(_cap: ::phoxal_bus::OwnerCap) -> Root {
                    Root
                }

                /// Root of the owner topic builder chain. `#[non_exhaustive]` so it
                /// cannot be constructed by a direct `internal::Root` literal - the
                /// ONLY entry is `internal::new(cap)`, which closes the L2 owner-cap
                /// gate (a bare `Root` would otherwise reach `.node().leaf()` with no
                /// cap).
                #[non_exhaustive]
                pub struct Root;
                impl Root {
                    #owner_root_methods
                }

                #owner_builder_mods
            }
        }
    })
}

/// One node along a builder path: its literal name (a key segment) and, if the
/// node is dynamic, the variable field it binds. The path is enough to build both
/// the carried-field set and the `format!` key for a leaf.
#[derive(Clone)]
struct NodeSeg {
    name: Ident,
    var: Option<Ident>,
}

/// The method on a parent builder (or `Root`) that enters `node`'s builder. A
/// static node takes no args; a dynamic node takes its var as `impl Display`. The
/// returned builder carries all vars bound so far plus this node's (if any).
fn node_entry_method(node: &Node) -> TokenStream {
    let name = &node.name;
    let name_str = name.to_string();
    let target = quote!(#name::Builder);
    match &node.var {
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

/// Emit the builder module for `node` (and recursively its children) on `side`.
/// `ancestors` is the chain of nodes from the version root down to (but excluding)
/// `node`, in order. The node's depth under the version is `ancestors.len() + 1`.
/// Each builder is a struct that stores every in-scope var as a `String`; leaf
/// methods format the key from those fields and brand the returned `Topic` per
/// `side` (the same structure/keys on both sides; only the leaf brand differs).
fn expand_builder_module(
    node: &Node,
    ancestors: &[NodeSeg],
    side: Side,
) -> syn::Result<TokenStream> {
    let name = &node.name;
    let name_str = name.to_string();
    let depth = ancestors.len() + 1;

    // Full node path (root → node) and the variables in scope (ancestors' + this
    // node's, in order).
    let mut path: Vec<NodeSeg> = ancestors.to_vec();
    path.push(NodeSeg {
        name: name.clone(),
        var: node.var.clone(),
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
    // (depth 1) and `super::Builder` for a nested node.
    let parent_builder_ty = if depth == 1 {
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
    let ctor = match &node.var {
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
    for topic in &node.topics {
        let leaf = &topic.leaf;
        let kind_ty = builder_leaf_kind(topic, &path, depth, side);
        let (fmt_str, doc_key) = builder_leaf_key_parts(&path, &topic.leaf);
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
    for child in &node.children {
        child_methods.extend(node_entry_method(child));
        child_mods.extend(expand_builder_module(child, &path, side)?);
    }

    let builder_doc = format!("Topic builder for the `{name_str}` node.");
    Ok(quote! {
        pub mod #name {
            #[doc = #builder_doc]
            // `#[non_exhaustive]` blocks cross-crate construction by struct literal,
            // so a node builder (incl. an empty static-node `Builder`) is only
            // reachable through the chain from `topic::new()` / `internal::new(cap)`.
            // Without this, an owner-side empty `Builder {}` could be built directly,
            // bypassing the L2 owner-cap gate.
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
    })
}

/// The branded `Kind` type for a builder leaf, side-aware (L1, plan #00).
///
/// Each body path resolves from inside the builder module up to the version root
/// and back down to the node module holding the body. A builder at `depth` (node
/// depth under the version) is nested `depth` levels under `topic` on the CLIENT
/// side, so reaching the version root is `depth + 1` supers (`depth` builder mods
/// plus the `topic` mod); on the OWNER side the whole tree sits one level deeper
/// under `topic::internal`, so it is `depth + 2`. From the version root, descend
/// the node-module path (`n1::n2::…::nk`).
///
/// The brand is picked from `(role, side)`:
///
/// - `command`: client publishes (`Publish`), owner subscribes (`Subscribe`).
/// - `state`: client subscribes (`Subscribe`), owner publishes (`Publish`).
/// - `query`: client asks (`AskQuery`), owner serves (`ServeQuery`).
fn builder_leaf_kind(topic: &TopicDef, path: &[NodeSeg], depth: usize, side: Side) -> TokenStream {
    let supers_to_root = match side {
        Side::Client => depth + 1,
        Side::Owner => depth + 2,
    };
    let up = supers(supers_to_root);
    let node_path: Vec<&Ident> = path.iter().map(|s| &s.name).collect();
    let body_path = |body: &Ident| quote! { #up #(#node_path::)* #body };
    match &topic.kind {
        TopicKind::PubSub(body) => {
            let b = body_path(body);
            // `command` and `state` share the pub/sub wire shape but invert which
            // side publishes vs subscribes; the role + side pick the brand.
            match (topic.role, side) {
                (TopicRole::Command, Side::Client) | (TopicRole::State, Side::Owner) => {
                    quote! { ::phoxal_bus::Publish<#b> }
                }
                (TopicRole::State, Side::Client) | (TopicRole::Command, Side::Owner) => {
                    quote! { ::phoxal_bus::Subscribe<#b> }
                }
                // A `query` role never carries a `PubSub` kind (the parser pairs
                // `query` with `TopicKind::Query`); fall back to the client view.
                (TopicRole::Query, _) => quote! { ::phoxal_bus::Subscribe<#b> },
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

/// Build a leaf's key in two forms: the `format!` template (literal node-name
/// segments, `{}` for each dynamic var, then `/leaf`) and the human-readable
/// `{var}`-placeholder doc key. Both are derived from the node path so the runtime
/// key and the documented key stay in lockstep.
fn builder_leaf_key_parts(path: &[NodeSeg], leaf: &Ident) -> (String, String) {
    let mut fmt_segs = Vec::new();
    let mut doc_segs = Vec::new();
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
    let leaf = leaf.to_string();
    (
        format!("{}/{}", fmt_segs.join("/"), leaf),
        format!("{}/{}", doc_segs.join("/"), leaf),
    )
}

/// Force every named field of a macro-declared body struct to `pub` so runtime
/// code in other modules can construct and read the wire body directly.
fn with_pub_fields_struct(mut item: ItemStruct) -> ItemStruct {
    if let syn::Fields::Named(named) = &mut item.fields {
        for field in &mut named.named {
            field.vis = syn::Visibility::Public(syn::token::Pub::default());
        }
    }
    item
}

#[cfg(test)]
mod tests {
    use super::expand;
    use quote::quote;

    /// `extends` must reject a same-named node that flips its `(var)`-ness, since
    /// that would silently rewrite every descendant topic key across versions.
    #[test]
    fn extends_rejects_var_ness_flip() {
        let input = quote! {
            version a { comp(instance) { struct S { x: u8 } topic s: state S; } }
            version b extends a { comp { struct S { x: u8, y: u8 } topic s: state S; } }
        };
        let err = expand(input).expect_err("a var-ness flip across `extends` must be rejected");
        assert!(
            err.to_string().contains("redeclares its dynamic variable"),
            "unexpected error: {err}"
        );
    }

    /// A child node that keeps the parent's `(var)`-ness overlays cleanly.
    #[test]
    fn extends_accepts_matching_var_ness() {
        let input = quote! {
            version a { comp(instance) { motor(cap) { enum C { Stop } topic command: command C; } } }
            version b extends a {
                comp(instance) { motor(cap) { enum C { Stop, Go } topic command: command C; } }
            }
        };
        assert!(expand(input).is_ok());
    }
}
