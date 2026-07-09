//! `phoxal_api_tree!` — the single API layer (D60/D61/D1).
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
//! `topic self: state <Body>;` binds the body to the node path itself instead of
//! appending a leaf segment, for framework infrastructure topics such as
//! `logs/{participant_id}`.
//! A version may be prefixed with `preview`; that emits the module at its final
//! path behind the per-generation `preview-y2026_N` Cargo feature and records
//! `ApiVersion::IS_PREVIEW = true` without changing the wire shape. `preview` is
//! the only per-version lifecycle marker: there is no `extends` (removed, D1) - a
//! generation is a standalone, sparse batch (target model #3), never a copy or an
//! override of an earlier one.
//!
//! ```text
//! phoxal_api_tree! {
//!     version y2026_1 {
//!         drive {                                  // static node
//!             struct Target { linear_x_mps: f32, angular_z_radps: f32 }
//!             topic target: command Target;        // key y2026_1/drive/target
//!             struct State { /* … */ }
//!             topic state: state State;            // owner-published telemetry
//!         }
//!         component(instance) {                    // literal "component" + var {instance}
//!             motor(capability) {                  // literal "motor" + var {capability}
//!                 enum Command { Velocity(f32), Torque(f32), Stop }
//!                 topic command: command Command;
//!                 // path   api::y2026_1::component::motor::Command
//!                 // key    y2026_1/component/{instance}/motor/{capability}/command
//!             }
//!         }
//!     }
//!     preview version y2026_2 {
//!         // a standalone, sparse batch: only what is minted in this generation,
//!         // never a copy of y2026_1.
//!         battery { struct State { soc: f32 } topic state: state State; }
//!     }
//! }
//! ```
//!
//! Each `version` becomes a `pub mod y2026_N` carrying a marker `enum Api {}`
//! (`ApiVersion`), a nested `pub mod` per node holding that node's version-local
//! bodies (plain serde types, no `{"v":…}` wrapper — D62) and their
//! `ContractBody` impls, plus an api-local `topic` builder module.
//!
//! **Wire identity is the generation-qualified key, not a transitive-shape hash
//! (D1).** The generation is folded into `ContractBody::TOPIC`: a contract's
//! identity is its version-qualified name (`y2026_1::drive::Target`), and that
//! name is real on the wire because the key carries it too
//! (`y2026_1/drive/target`). Two participants interoperate on a contract iff
//! they use the exact same version-qualified name - enforced by the type system
//! (the `Api` bound) and realized on the wire by the key, which makes two
//! differently-versioned contracts physically incapable of colliding. There is
//! no `SCHEMA_ID`/`FAMILY` and no cross-generation `extends`: a released
//! contract type is immutable, and changing it means minting a new name in the
//! current (or a new) generation, never overlaying the old one.
//!
//! # Self-contained absolute paths (no depth-counted `super::`)
//!
//! Every generated module - a node's own type module, or a leaf's topic-builder
//! module, at any depth on either the client or the owner side - needs to name two
//! things that live elsewhere in the SAME invocation's output: the version's `Api`
//! marker, and (builder modules only) the body type declared in the parallel
//! type-tree branch for the same node. Neither reference may assume WHERE
//! `phoxal_api_tree!` itself was invoked (crate root for the one production call
//! in `phoxal-api/src/lib.rs`; nested inside a `#[cfg(test)] mod tests` submodule
//! for the fixtures in `phoxal-api/src/tests.rs`) - a proc-macro has no reliable
//! way to learn its own module path, so a hardcoded `::phoxal_api::y2026_N::…`
//! prefix is wrong the moment the invocation is not at the crate root, and simply
//! is not an option here.
//!
//! Instead, every reference is built from a chain of single, ALWAYS-VALID hops to
//! the reference's own immediate parent module - never a multi-hop count derived
//! from how deep the referencing node happens to be nested. Two hidden,
//! `#[doc(hidden)]` re-exports carry this:
//!
//! - `__PhoxalApiMarker` aliases `Api` once, in the version module
//!   (`pub use self::Api as __PhoxalApiMarker;`). Every node module, at any depth,
//!   re-exports its own parent's copy one hop up
//!   (`pub use super::__PhoxalApiMarker;`), so by induction every node module, no
//!   matter how deep, has a local `self::__PhoxalApiMarker` it can bind
//!   `ContractBody::Api` to.
//! - `__phoxal_type_root` is seeded per top-level node in `topic`
//!   (`pub use super::<node> as __phoxal_type_root_<node>;`, one hop from `topic`
//!   to the version module that holds the type tree), then forwarded one hop into
//!   `topic::internal` under the same name, then every builder module along that
//!   node's subtree, client or owner side, any depth, imports it from its own
//!   parent under the uniform local name `__phoxal_type_root`. A leaf's body
//!   reference is then `self::__phoxal_type_root::<rest of the node path>::<Body>`:
//!   the hop count is always exactly one, never counted against the node's
//!   authored depth or which side is being built.
//!
//! The result: no reference anywhere in the generated tree depends on a computed
//! nesting depth, so lifting a node deeper (or invoking `phoxal_api_tree!` from a
//! more deeply nested module, as the `reused_var_name` / `standalone_second_generation`
//! fixtures in `phoxal-api/src/tests.rs` do) cannot desynchronize a path. The
//! builder's OWN parent-chaining (`super::Root` / `super::Builder` in
//! [`expand_builder_module`]) is intentionally left as a direct `super::`: it
//! names this builder's immediate enclosing module, which is by construction
//! always exactly one hop away regardless of depth, so it was never part of the
//! depth-counted problem this section solves.
//!
//! # No path-based rename
//!
//! An earlier design draft considered a `rename = "seg"` attribute to decouple a
//! node/leaf's wire key segment from its Rust identifier, so the Rust API could be
//! refactored (renaming or moving a type in the node tree) without moving the wire
//! key. Under the CURRENT model that capability is redundant, so it is
//! deliberately not implemented:
//!
//! - A released (non-`preview`) `version` span is immutable by policy, enforced by
//!   the release-PR frozen-generation check (`xtask/src/api/frozen_generation.rs`):
//!   it diffs the exact DSL source text against the last release tag and fails the
//!   PR if a single byte of a frozen span moved. Renaming an identifier inside a
//!   frozen span is exactly the kind of edit that check exists to block - a
//!   `rename` attribute could not legally be added there anyway, so it buys
//!   nothing for released contracts.
//! - A `preview` span carries no immutability promise at all: every identifier in
//!   it, including node/leaf names, can be edited freely with no external
//!   consumer to break, so there is no window where a Rust name and a wire name
//!   would need to diverge.
//! - The whole point of D1's move away from `schema_id`/`FAMILY` is that identity
//!   collapses onto ONE axis - the version-qualified Rust path IS the wire key
//!   (`y2026_1::drive::Target` <-> `y2026_1/drive/target`). A `rename` attribute
//!   would reopen a second axis (Rust name vs. wire name) for exactly the
//!   contracts where the model guarantees they can never need to diverge.
//!
//! If a future generation needs a differently-worded wire segment than its Rust
//! name reads naturally, the sparse-generation model already provides the answer:
//! mint it under the wire name that reads well from the start, in the current (or
//! a new) generation - there is no cost to choosing the wire-facing name up front.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::{Ident, ItemEnum, ItemStruct, Token};

use crate::util::body_derives;

mod kw {
    syn::custom_keyword!(preview);
    syn::custom_keyword!(version);
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
    is_preview: bool,
    name: Ident,
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

#[derive(Clone)]
struct TopicDef {
    leaf: TopicLeaf,
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

#[derive(Clone)]
enum TopicLeaf {
    Named(Ident),
    Node,
}

impl TopicLeaf {
    fn method_ident(&self) -> Ident {
        match self {
            TopicLeaf::Named(ident) => ident.clone(),
            TopicLeaf::Node => quote::format_ident!("topic"),
        }
    }
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

struct ManifestGeneration {
    name: String,
    is_preview: bool,
    contracts: Vec<ManifestContract>,
}

struct ManifestContract {
    /// Version-qualified contract identity, e.g. `"y2026_1::drive::Target"`
    /// (D1: the generation is part of the name, not a separate axis).
    family: String,
    /// Generation-qualified wire key, e.g. `"y2026_1/drive/target"`.
    topic: String,
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
        let is_preview = if input.peek(kw::preview) {
            input.parse::<kw::preview>()?;
            true
        } else {
            false
        };
        input.parse::<kw::version>()?;
        let name: Ident = input.parse()?;
        let body;
        syn::braced!(body in input);
        let mut nodes = Vec::new();
        while !body.is_empty() {
            nodes.push(body.parse()?);
        }
        Ok(Version {
            is_preview,
            name,
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
        let leaf = if input.peek(Token![self]) {
            input.parse::<Token![self]>()?;
            TopicLeaf::Node
        } else {
            TopicLeaf::Named(input.parse()?)
        };
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
        let mut manifest_generations = Vec::new();
        let mut seen_names = std::collections::HashSet::new();

        // Each version is a standalone, sparse batch (D1/target model #3): no
        // `extends`, no cross-generation node overlay. A generation contains only
        // the contracts minted in that batch.
        for version in &self.versions {
            if !seen_names.insert(version.name.to_string()) {
                return Err(syn::Error::new_spanned(
                    &version.name,
                    format!(
                        "duplicate API version `{}` in the same phoxal_api_tree! invocation",
                        version.name
                    ),
                ));
            }
            manifest_generations.push(ManifestGeneration {
                name: version.name.to_string(),
                is_preview: version.is_preview,
                contracts: contract_manifest_entries(&version.name.to_string(), &version.nodes)?,
            });
            out.extend(expand_version(version)?);
        }
        let manifest = expand_contract_manifest(&manifest_generations);
        Ok(quote! {
            #manifest
            #out
        })
    }
}

fn expand_contract_manifest(generations: &[ManifestGeneration]) -> TokenStream {
    let generation_entries = generations.iter().map(|generation| {
        let name = &generation.name;
        let is_preview = generation.is_preview;
        let contracts = generation.contracts.iter().map(|contract| {
            let family = &contract.family;
            let topic = &contract.topic;
            quote! {
                ApiContractManifestContract {
                    family: #family,
                    topic: #topic,
                }
            }
        });
        quote! {
            ApiContractManifestGeneration {
                name: #name,
                is_preview: #is_preview,
                contracts: &[#(#contracts),*],
            }
        }
    });

    quote! {
        /// One generated API generation in the contract manifest.
        #[doc(hidden)]
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct ApiContractManifestGeneration {
            pub name: &'static str,
            pub is_preview: bool,
            pub contracts: &'static [ApiContractManifestContract],
        }

        /// One generated contract in the contract manifest. `family` is the
        /// version-qualified contract identity (D1); `topic` is its
        /// generation-qualified wire key. There is no `schema_id`: the name
        /// itself is the whole identity (D1).
        #[doc(hidden)]
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct ApiContractManifestContract {
            pub family: &'static str,
            pub topic: &'static str,
        }

        /// Generated contract manifest for xtask lifecycle checks.
        #[doc(hidden)]
        pub const API_CONTRACT_MANIFEST: &[ApiContractManifestGeneration] = &[#(#generation_entries),*];
    }
}

fn contract_manifest_entries(version: &str, nodes: &[Node]) -> syn::Result<Vec<ManifestContract>> {
    let mut contracts = Vec::new();
    collect_contract_manifest_entries(version, nodes, "", "", &mut contracts);
    contracts.sort_by(|left, right| {
        left.family
            .cmp(&right.family)
            .then_with(|| left.topic.cmp(&right.topic))
    });
    Ok(contracts)
}

fn collect_contract_manifest_entries(
    version: &str,
    nodes: &[Node],
    family_prefix: &str,
    key_prefix: &str,
    contracts: &mut Vec<ManifestContract>,
) {
    for node in nodes {
        let name = node.name.to_string();
        let family_path = join_seg(family_prefix, "::", &name);
        let key_seg = match &node.var {
            Some(var) => format!("{}/{{{}}}", name, var),
            None => name.clone(),
        };
        let node_key_prefix = join_seg(key_prefix, "/", &key_seg);

        for topic in &node.topics {
            let topic_key = format!("{version}/{}", topic_key(&node_key_prefix, &topic.leaf));
            match &topic.kind {
                TopicKind::PubSub(body) => {
                    contracts.push(ManifestContract {
                        family: format!("{version}::{family_path}::{body}"),
                        topic: topic_key,
                    });
                }
                TopicKind::Query { request, response } => {
                    contracts.push(ManifestContract {
                        family: format!("{version}::{family_path}::{request}"),
                        topic: topic_key.clone(),
                    });
                    contracts.push(ManifestContract {
                        family: format!("{version}::{family_path}::{response}"),
                        topic: topic_key,
                    });
                }
            }
        }

        collect_contract_manifest_entries(
            version,
            &node.children,
            &family_path,
            &node_key_prefix,
            contracts,
        );
    }
}

fn expand_version(version: &Version) -> syn::Result<TokenStream> {
    let mod_name = &version.name;
    let id = version.name.to_string();
    let is_preview = version.is_preview;
    let feature_name = format!("preview-{id}");
    let nodes = &version.nodes;

    // Node modules (types + ContractBody impls), recursive. The family prefix
    // (`::`-joined node names) and the key prefix (`/`-joined `name` or
    // `name/{var}` segments) are threaded down the walk. `id` (the version name)
    // is threaded down too so every emitted `TOPIC` is generation-qualified (D1):
    // the generation is folded into the wire key, so different versioned
    // contracts can never collide.
    let mut node_mods = TokenStream::new();
    for node in nodes {
        node_mods.extend(expand_node_module(node, &id, "", "")?);
    }

    let topic_mod = expand_topic_module(&id, nodes)?;

    let module_attrs = if is_preview {
        quote! {
            #[cfg(feature = #feature_name)]
        }
    } else {
        TokenStream::new()
    };
    let module_doc = if is_preview {
        format!(
            "Preview dated API version `{id}`. This final-path module is available only with the `{feature_name}` Cargo feature."
        )
    } else {
        format!("Dated API version `{id}` - version-local wire bodies + topics.")
    };

    Ok(quote! {
        #[doc = #module_doc]
        #module_attrs
        pub mod #mod_name {
            /// Zero-variant marker identifying this API version (D60).
            #[derive(Clone, Copy, Debug)]
            pub enum Api {}
            impl ::phoxal_bus::ApiVersion for Api {
                const ID: &'static str = #id;
                const IS_PREVIEW: bool = #is_preview;
            }

            // Self-contained absolute-path anchor (position-independent
            // regardless of where `phoxal_api_tree!` is invoked - crate root or a
            // nested test-fixture module, D1's "self-contained" redesign). Every
            // node module and every topic-builder module below - no matter how
            // deep the tree - re-exports this ONE hop from its own parent, so any
            // of them reaches `Api` through a purely local `self::__PhoxalApiMarker`
            // that never needs a supers count tied to its nesting depth.
            #[doc(hidden)]
            pub use self::Api as __PhoxalApiMarker;

            #node_mods

            #topic_mod
        }
    })
}

/// Emit a `pub mod <name>` for a node under the version. The module carries the
/// node's types, the `ContractBody` impls for its topics, and — recursively —
/// its child node modules. Variables never appear in the module path (D61).
///
/// `version` is the generation name (e.g. `"y2026_1"`), threaded down so every
/// emitted `TOPIC` is generation-qualified (D1). `family_prefix` is the
/// `::`-joined ancestor node names (empty at the root); `key_prefix` is the
/// `/`-joined ancestor key segments (`name` or `name/{var}`, empty at the root).
/// The node appends its own contribution to each.
fn expand_node_module(
    node: &Node,
    version: &str,
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
        // The generation-qualified wire key (D1): folding the version in here is
        // what makes different versioned names physically distinct Zenoh keys.
        let key = format!("{version}/{}", topic_key(&node_key_prefix, &topic.leaf));
        let role = topic.role.bus_variant();
        match &topic.kind {
            TopicKind::PubSub(body) => {
                // The role rides as an inherent `#[doc(hidden)] pub const ROLE` on
                // the body: additive surface that does not touch `ContractBody`.
                // The side-branded builders are what enforce owner/client (L1);
                // the role is not yet emitted by `emit-apis` (a later increment of
                // plan #00).
                impls.extend(quote! {
                    impl ::phoxal_bus::ContractBody for #body {
                        type Api = self::__PhoxalApiMarker;
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
                impls.extend(quote! {
                    impl ::phoxal_bus::ContractBody for #request {
                        type Api = self::__PhoxalApiMarker;
                        const TOPIC: &'static str = #key;
                    }
                    impl ::phoxal_bus::ContractBody for #response {
                        type Api = self::__PhoxalApiMarker;
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
            version,
            &family_path,
            &node_key_prefix,
        )?);
    }

    Ok(quote! {
        pub mod #name {
            //! Version-local bodies for the `#name_str` node.

            // Forward the version root's `Api` marker down exactly one hop from
            // this node's own parent (the version module for a top-level node, or
            // the parent node module for a nested one). Every node module - at any
            // depth - carries this same single-hop re-export, so `Api` is always
            // reachable as `self::__PhoxalApiMarker` without computing how deep
            // this node sits (self-contained absolute path, D1).
            #[doc(hidden)]
            pub use super::__PhoxalApiMarker;

            #types
            #impls
            #child_mods
        }
    })
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

fn topic_key(node_key_prefix: &str, leaf: &TopicLeaf) -> String {
    match leaf {
        TopicLeaf::Named(leaf) => format!("{}/{}", node_key_prefix, leaf),
        TopicLeaf::Node => node_key_prefix.to_string(),
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

/// The name of the hidden alias, seeded once in `topic` per top-level node, that
/// re-exports that node's type-tree module (e.g. `component`) under a name that
/// cannot collide with the SAME-named builder submodule `topic` also declares for
/// it. Builder modules import it (and re-forward it downward, see
/// [`expand_builder_module`]) as `__phoxal_type_root`.
fn type_root_alias_ident(node_name: &Ident) -> Ident {
    quote::format_ident!("__phoxal_type_root_{}", node_name)
}

/// Emit the api-local `topic` builder module with BOTH side trees (L1).
///
/// The PUBLIC client tree lives directly under `topic` (`topic::new()` + a builder
/// module per node); the OWNER tree lives under `topic::internal`
/// (`topic::internal::new(cap)` + the same builder modules, one level deeper). Both
/// mirror the node tree and format identical keys - only the leaf brand differs by
/// side. A dynamic node's method takes its variable as `impl Display` and carries
/// it forward; a leaf method formats the final key from the carried vars.
///
/// Self-contained absolute paths (D1): a builder leaf needs to name a body type
/// that lives in the PARALLEL type-tree hanging off the same version module
/// (`topic::component::motor::Builder` needs `component::motor::Command`). Rather
/// than counting `super::` hops back to the version root and down again per leaf,
/// `topic` seeds one hidden alias per top-level node
/// (`#[doc(hidden)] pub use super::component as __phoxal_type_root_component;`,
/// a single, always-valid hop since `topic` is a direct child of the version
/// module) and `internal` re-forwards each of them one hop further. Every builder
/// module under either side then imports its own top-level node's alias - a
/// single hop from its immediate parent - under the uniform local name
/// `__phoxal_type_root`, and deeper builder modules just forward THAT one hop at
/// a time. A leaf reference is then always `self::__phoxal_type_root::…::Body`:
/// no supers count, no dependency on how deep the node was authored.
fn expand_topic_module(version: &str, nodes: &[Node]) -> syn::Result<TokenStream> {
    let mut client_root_methods = TokenStream::new();
    let mut client_builder_mods = TokenStream::new();
    let mut owner_root_methods = TokenStream::new();
    let mut owner_builder_mods = TokenStream::new();
    let mut type_root_seeds = TokenStream::new();
    let mut type_root_forwards = TokenStream::new();
    for node in nodes {
        let name = &node.name;
        let alias = type_root_alias_ident(name);

        client_root_methods.extend(node_entry_method(node));
        client_builder_mods.extend(expand_builder_module(node, version, &[], Side::Client)?);
        owner_root_methods.extend(node_entry_method(node));
        owner_builder_mods.extend(expand_builder_module(node, version, &[], Side::Owner)?);

        type_root_seeds.extend(quote! {
            #[doc(hidden)]
            pub use super::#name as #alias;
        });
        type_root_forwards.extend(quote! {
            #[doc(hidden)]
            pub use super::#alias;
        });
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

            // Per-top-level-node type-tree aliases (self-contained absolute
            // paths, D1): seeded here because `topic` is always exactly one hop
            // from the version module that holds the type tree.
            #type_root_seeds

            #client_builder_mods

            /// Owner-side topic builders (L1 + L2, plan #00). `internal::new(cap)...`
            /// is the deliberate, greppable owner opt-in: a participant acquires the
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

                // Forward each top-level node's type-tree alias one more hop, from
                // `topic` into `internal` (still a single, always-valid hop).
                #type_root_forwards

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
    version: &str,
    ancestors: &[NodeSeg],
    side: Side,
) -> syn::Result<TokenStream> {
    let name = &node.name;
    let name_str = name.to_string();

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
    // (`ancestors` empty) and `super::Builder` for a nested one. Unlike the
    // type-tree cross-reference below, this is a direct, single-hop reference to
    // this builder's own immediate parent module - it is already
    // depth-independent (always exactly one `super::`, never counted), so it is
    // not part of the self-contained-path rework.
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
        let leaf = topic.leaf.method_ident();
        let kind_ty = builder_leaf_kind(topic, &path, side);
        let (fmt_str, doc_key) = builder_leaf_key_parts(version, &path, &topic.leaf);
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
        child_mods.extend(expand_builder_module(child, version, &path, side)?);
    }

    // Self-contained absolute path to this top-level node's type-tree (D1): at the
    // top of a top-level node's builder subtree (`ancestors` empty) import the
    // alias `expand_topic_module` seeded one hop up (in `topic` for the client
    // side, in `topic::internal` for the owner side - both are that alias's direct
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
    Ok(quote! {
        pub mod #name {
            #type_root_import

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
/// The body path is built from `self::__phoxal_type_root` (this top-level node's
/// type-tree alias, forwarded one hop at a time down from `topic`/`topic::internal`;
/// see [`expand_topic_module`] and [`expand_builder_module`]), followed by the node
/// path's segments after the top-level one (which the alias already denotes), then
/// the body ident. This is a fixed-shape reference that never depends on how deep
/// the node was authored or on which side (client/owner) is being built.
///
/// The brand is picked from `(role, side)`:
///
/// - `command`: client publishes (`Publish`), owner subscribes (`Subscribe`).
/// - `state`: client subscribes (`Subscribe`), owner publishes (`Publish`).
/// - `query`: client asks (`AskQuery`), owner serves (`ServeQuery`).
fn builder_leaf_kind(topic: &TopicDef, path: &[NodeSeg], side: Side) -> TokenStream {
    // `path[0]` is the top-level node - exactly what `__phoxal_type_root` already
    // aliases - so only the segments AFTER it need to be descended.
    let rest_path: Vec<&Ident> = path[1..].iter().map(|s| &s.name).collect();
    let body_path = |body: &Ident| quote! { self::__phoxal_type_root #(::#rest_path)* :: #body };
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

/// Build a leaf's key in two forms: the `format!` template (the generation as a
/// literal leading segment, then literal node-name segments, `{}` for each
/// dynamic var, optionally then `/leaf`) and the human-readable
/// `{var}`-placeholder doc key. Both are derived from the node path so the
/// concrete key and the documented key stay in lockstep with
/// `ContractBody::TOPIC` (D1).
fn builder_leaf_key_parts(version: &str, path: &[NodeSeg], leaf: &TopicLeaf) -> (String, String) {
    let mut fmt_segs = vec![version.to_string()];
    let mut doc_segs = vec![version.to_string()];
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
    match leaf {
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

/// Force every named field of a macro-declared body struct to `pub` so participant
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
    use proc_macro2::TokenStream;
    use quote::quote;

    fn compact_tokens(tokens: TokenStream) -> String {
        tokens
            .to_string()
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ")
    }

    #[test]
    fn topic_and_family_are_generation_qualified() {
        let expanded = compact_tokens(
            expand(quote! {
                version y2026_1 {
                    drive {
                        struct Target { linear_x_mps: f32 }
                        topic target: command Target;
                    }
                }
            })
            .expect("tree expands"),
        );
        assert!(
            expanded.contains("const TOPIC : & 'static str = \"y2026_1/drive/target\""),
            "TOPIC must fold the generation into the wire key (D1): {expanded}"
        );
        assert!(
            !expanded.contains("SCHEMA_ID"),
            "there is no schema_id left to emit: {expanded}"
        );
        assert!(
            !expanded.contains("FAMILY"),
            "ContractBody no longer carries FAMILY (D1): {expanded}"
        );
    }

    #[test]
    fn dynamic_node_topic_is_generation_and_var_qualified() {
        let expanded = compact_tokens(
            expand(quote! {
                version y2026_1 {
                    component(instance) {
                        motor(capability) {
                            enum Command { Stop }
                            topic command: command Command;
                        }
                    }
                }
            })
            .expect("tree expands"),
        );
        assert!(
            expanded.contains(
                "const TOPIC : & 'static str = \"y2026_1/component/{instance}/motor/{capability}/command\""
            ),
            "dynamic-node TOPIC must carry both the generation and the {{var}} placeholders: {expanded}"
        );
    }

    #[test]
    fn topic_builder_keys_are_generation_qualified() {
        let expanded = compact_tokens(
            expand(quote! {
                version y2026_1 {
                    drive {
                        struct Target { linear_x_mps: f32 }
                        topic target: command Target;
                    }
                }
            })
            .expect("tree expands"),
        );
        assert!(
            expanded.contains("Topic :: new_static (\"y2026_1/drive/target\")"),
            "the api-local topic builder must build the same generation-qualified key as \
             ContractBody::TOPIC: {expanded}"
        );
    }

    #[test]
    fn duplicate_version_names_in_one_invocation_are_rejected() {
        let err = expand(quote! {
            version y2026_1 { sample { struct Body { value: u8 } topic body: state Body; } }
            version y2026_1 { sample { struct Body { value: u8 } topic body: state Body; } }
        })
        .expect_err("a duplicate version name must be rejected");
        assert!(
            err.to_string().contains("duplicate API version"),
            "unexpected error: {err}"
        );
    }

    /// `preview` is independent of `extends` (which no longer exists, D1/target
    /// model #3): a standalone preview version with no parent must still emit
    /// the final-path module behind its per-generation feature gate.
    #[test]
    fn standalone_preview_version_emits_final_path_feature_gate_and_lifecycle_const() {
        let expanded = compact_tokens(
            expand(quote! {
                preview version y2026_2 {
                    sample {
                        struct Body { value: u8 }
                        topic body: state Body;
                    }
                }
            })
            .expect("preview tree expands"),
        );

        assert!(
            expanded.contains("pub mod y2026_2"),
            "preview generation must be emitted at its final path: {expanded}"
        );
        assert!(
            !expanded.contains("pub mod preview"),
            "preview generation must not be nested under a preview module: {expanded}"
        );
        assert!(
            expanded.contains("# [cfg (feature = \"preview-y2026_2\")]"),
            "preview generation must be gated by its per-generation feature: {expanded}"
        );
        assert!(
            expanded.contains("Preview dated API version `y2026_2`"),
            "preview generation should carry a discoverable doc note: {expanded}"
        );
        assert!(
            expanded.contains("const IS_PREVIEW : bool = true ;"),
            "preview ApiVersion must record IS_PREVIEW = true: {expanded}"
        );
        assert!(
            expanded.contains("const TOPIC : & 'static str = \"y2026_2/sample/body\""),
            "a preview generation's wire key is generation-qualified exactly like a \
             released one: {expanded}"
        );
    }

    #[test]
    fn preview_lifecycle_is_wire_neutral_for_contract_identity() {
        let preview = quote! {
            preview version y2026_2 {
                sample {
                    struct Body { value: u8, label: Option<String> }
                    topic body: state Body;
                }
            }
        };
        let promoted = quote! {
            version y2026_2 {
                sample {
                    struct Body { value: u8, label: Option<String> }
                    topic body: state Body;
                }
            }
        };

        let preview_expanded = compact_tokens(expand(preview).expect("preview tree expands"));
        let promoted_expanded = compact_tokens(expand(promoted).expect("promoted tree expands"));

        // Preview lifecycle has no wire effect (D1): with no schema_id left to
        // compare, the generation-qualified TOPIC itself is the identity, and it
        // must be identical whether or not the generation is still in preview.
        assert!(preview_expanded.contains("\"y2026_2/sample/body\""));
        assert!(promoted_expanded.contains("\"y2026_2/sample/body\""));
    }

    #[test]
    fn expansion_emits_root_contract_manifest_for_xtask() {
        let expanded = compact_tokens(
            expand(quote! {
                version y2026_1 {
                    sample {
                        struct Body { value: u8 }
                        topic body: state Body;
                    }
                }
                preview version y2026_2 {
                    sample {
                        struct Body { value: u8 }
                        topic body: state Body;
                    }
                }
            })
            .expect("tree expands"),
        );

        assert!(
            expanded.contains("pub const API_CONTRACT_MANIFEST"),
            "root manifest const should be emitted: {expanded}"
        );
        assert!(
            expanded.contains("name : \"y2026_2\""),
            "preview generation should be represented in the manifest: {expanded}"
        );
        assert!(
            expanded.contains("is_preview : true"),
            "manifest should carry preview lifecycle: {expanded}"
        );
        assert!(
            expanded.contains("family : \"y2026_1::sample::Body\""),
            "manifest family is the version-qualified contract identity (D1): {expanded}"
        );
        assert!(
            expanded.contains("topic : \"y2026_1/sample/body\""),
            "manifest topic is the generation-qualified wire key (D1): {expanded}"
        );
        assert!(
            expanded.contains("family : \"y2026_2::sample::Body\""),
            "each generation's contracts get their own version-qualified name: {expanded}"
        );
        assert!(
            expanded.contains("topic : \"y2026_2/sample/body\""),
            "each generation's contracts get their own generation-qualified key: {expanded}"
        );
        assert!(
            !expanded.contains("schema_id :"),
            "there is no schema_id field left in the manifest (D1): {expanded}"
        );
        assert!(
            !expanded.contains("extends :"),
            "there is no extends field left in the manifest: {expanded}"
        );
    }

    #[test]
    fn query_request_and_response_share_one_generation_qualified_topic() {
        let expanded = compact_tokens(
            expand(quote! {
                version y2026_1 {
                    asset {
                        struct GetRequest { path: String }
                        enum GetResponse { Missing }
                        topic get: query GetRequest => GetResponse;
                    }
                }
            })
            .expect("tree expands"),
        );
        assert_eq!(
            expanded
                .matches("const TOPIC : & 'static str = \"y2026_1/asset/get\"")
                .count(),
            2,
            "both the request and response bodies of a query topic share its \
             generation-qualified key: {expanded}"
        );
    }

    /// Self-contained absolute paths (no depth-counted `super::`): a node nested
    /// three levels deep must resolve `ContractBody::Api` and its builder-leaf
    /// body type through the single-hop forwarding chain
    /// (`__PhoxalApiMarker`/`__phoxal_type_root`), never through a `super::`
    /// chain whose length is computed from the node's authored depth. A
    /// regression back to depth-counted supers would need `super :: super` (or
    /// deeper) somewhere in this expansion; the self-contained scheme never
    /// does, on either the client or the owner builder tree.
    #[test]
    fn deeply_nested_dynamic_tree_never_emits_a_multi_hop_super_chain() {
        let expanded = compact_tokens(
            expand(quote! {
                version y2026_1 {
                    a(x) {
                        b(y) {
                            c(z) {
                                struct Body { v: u8 }
                                topic leaf: state Body;
                            }
                        }
                    }
                }
            })
            .expect("tree expands"),
        );

        assert!(
            !expanded.contains("super :: super"),
            "no reference should ever chain more than one `super::` hop, on \
             either the type-tree or the builder-tree side: {expanded}"
        );
        assert!(
            expanded.contains("const TOPIC : & 'static str = \"y2026_1/a/{x}/b/{y}/c/{z}/leaf\""),
            "the three-level dynamic path must still be fully generation- and \
             var-qualified: {expanded}"
        );
        assert!(
            expanded.contains("type Api = self :: __PhoxalApiMarker ;"),
            "ContractBody::Api must resolve through the forwarded, single-hop \
             alias at any depth: {expanded}"
        );
        assert!(
            expanded.contains("self :: __phoxal_type_root :: b :: c :: Body"),
            "a deeply nested builder leaf must reach its body type through the \
             forwarded type-root alias plus the remaining node path, not a \
             counted supers chain: {expanded}"
        );
    }
}
