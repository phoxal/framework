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
use sha2::{Digest, Sha256};
use syn::parse::{Parse, ParseStream};
use syn::punctuated::Punctuated;
use syn::spanned::Spanned;
use syn::{
    Attribute, Expr, Fields, GenericArgument, GenericParam, Ident, ItemEnum, ItemStruct, Lit,
    LitStr, Path, PathArguments, Token, Type, TypePath,
};

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
    let shapes = ShapeCatalog::new(nodes)?;

    // Node modules (types + ContractBody impls), recursive. Each top-level node is
    // at depth 1 under the version. The family prefix (`::`-joined node names) and
    // the key prefix (`/`-joined `name` or `name/{var}` segments) are threaded
    // down the walk.
    let mut node_mods = TokenStream::new();
    for node in nodes {
        node_mods.extend(expand_node_module(node, 1, "", "", &shapes)?);
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
    shapes: &ShapeCatalog,
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
                let schema_id = shapes.schema_id_for(&family_path, body)?;
                // The role rides as an inherent `#[doc(hidden)] pub const ROLE` on
                // the body: additive surface that does not touch `ContractBody`.
                // The side-branded builders are what enforce owner/client (L1);
                // the role is not yet emitted by `emit-apis` (a later increment of
                // plan #00).
                impls.extend(quote! {
                    impl ::phoxal_bus::ContractBody for #body {
                        type Api = #api_supers Api;
                        const FAMILY: &'static str = #family_const;
                        const SCHEMA_ID: &'static str = #schema_id;
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
                let req_schema_id = shapes.schema_id_for(&family_path, request)?;
                let resp_schema_id = shapes.schema_id_for(&family_path, response)?;
                impls.extend(quote! {
                    impl ::phoxal_bus::ContractBody for #request {
                        type Api = #api_supers Api;
                        const FAMILY: &'static str = #req_family;
                        const SCHEMA_ID: &'static str = #req_schema_id;
                        const TOPIC: &'static str = #key;
                    }
                    impl ::phoxal_bus::ContractBody for #response {
                        type Api = #api_supers Api;
                        const FAMILY: &'static str = #resp_family;
                        const SCHEMA_ID: &'static str = #resp_schema_id;
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
            shapes,
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

/// Resolved wire-shape catalog for one effective API version.
///
/// Canonical schema input is `phoxal.schema/v0\n` followed by this grammar:
///
/// ```text
/// body    = unit | newtype | tuple | struct | enum
/// struct  = "struct{" field* "}"
/// field   = "f" byte_len ":" utf8_name "=" ty ";"
/// enum    = "enum(" repr "){" variant* "}"
/// repr    = "external" | "internal(tag=" name ")"
///         | "adjacent(tag=" name ",content=" name ")" | "untagged"
/// variant = "v" byte_len ":" utf8_name "=" payload ";"
/// payload = unit | "newtype(" ty ")" | tuple | struct
/// newtype = "newtype(" ty ")"
/// tuple   = "tuple(" (ty ";")* ")"
/// ty      = nil | bool | string | bytes | integer | float | option | seq | array
///         | map | tuple | body
/// integer = "u8" | "u16" | "u32" | "u64" | "u128"
///         | "i8" | "i16" | "i32" | "i64" | "i128"
/// float   = "f32" | "f64"
/// option  = "option(" ty ")"
/// seq     = "seq(" ty ")"
/// array   = "array[" decimal_len "](" ty ")"
/// map     = "map(" ty "," ty ")"
/// name    = byte_len ":" utf8_name
/// ```
///
/// Fields stay in declared order and use their serde wire names, including
/// `rename` and `rename_all`. Enums include serde representation mode, variant
/// wire names, and payload shapes. Nested API-tree structs and enums are expanded
/// transitively, with generic type parameters substituted by their concrete type
/// arguments before expansion. Only understood serde attributes and known
/// wire-neutral attributes such as doc comments and simple derives are accepted.
/// Non-wire details such as Rust type identifiers when the wire shape has already
/// been expanded are excluded. The emitted `schema_id` is lower-hex
/// `sha256(canonical_string)`, truncated to the first 16 hex characters.
///
/// Invariant: the canonicalizer never produces a hash for a type whose wire shape
/// it has not provably modeled. Unmodeled types are compile errors, so silent path
/// mis-resolution cannot make two different wire shapes share a `schema_id`.
#[derive(Clone)]
struct ShapeCatalog {
    types: std::collections::HashMap<(String, String), TypeDef>,
}

impl ShapeCatalog {
    fn new(nodes: &[Node]) -> syn::Result<Self> {
        let mut catalog = ShapeCatalog {
            types: std::collections::HashMap::new(),
        };
        catalog.collect_nodes(nodes, "")?;
        Ok(catalog)
    }

    fn collect_nodes(&mut self, nodes: &[Node], family_prefix: &str) -> syn::Result<()> {
        for node in nodes {
            let name = node.name.to_string();
            let family_path = join_seg(family_prefix, "::", &name);
            for ty in &node.types {
                let key = (family_path.clone(), ty.ident().to_string());
                if self.types.insert(key, ty.clone()).is_some() {
                    return Err(syn::Error::new_spanned(
                        ty.ident(),
                        format!(
                            "duplicate type `{}` in API node `{family_path}` cannot have a stable schema_id",
                            ty.ident()
                        ),
                    ));
                }
            }
            self.collect_nodes(&node.children, &family_path)?;
        }
        Ok(())
    }

    fn schema_id_for(&self, scope: &str, ident: &Ident) -> syn::Result<String> {
        let mut stack = Vec::new();
        let subst = TypeSubst::new();
        let body = self.canonical_named(scope, ident, &[], &subst, &mut stack)?;
        let canonical = format!("phoxal.schema/v0\n{body}");
        Ok(schema_hash(&canonical))
    }

    fn canonical_named(
        &self,
        scope: &str,
        ident: &Ident,
        args: &[Type],
        subst: &TypeSubst,
        stack: &mut Vec<(String, String)>,
    ) -> syn::Result<String> {
        let key = (scope.to_string(), ident.to_string());
        let ty = self.types.get(&key).ok_or_else(|| {
            syn::Error::new_spanned(
                ident,
                format!("topic body `{ident}` is not declared in API node `{scope}`"),
            )
        })?;
        if stack.contains(&key) {
            return Err(syn::Error::new_spanned(
                ident,
                format!("recursive wire type `{ident}` is not supported by schema_id"),
            ));
        }

        let params = type_params(ty)?;
        if params.len() != args.len() {
            return Err(syn::Error::new_spanned(
                ident,
                format!(
                    "type `{ident}` expects {} generic argument(s), got {}",
                    params.len(),
                    args.len()
                ),
            ));
        }
        let mut local_subst = TypeSubst::new();
        for (param, arg) in params.into_iter().zip(args.iter()) {
            local_subst.insert(param, substitute_type(arg, subst));
        }

        stack.push(key);
        let result = match ty {
            TypeDef::Struct(item) => self.canonical_struct(item, scope, &local_subst, stack),
            TypeDef::Enum(item) => self.canonical_enum(item, scope, &local_subst, stack),
        };
        stack.pop();
        result
    }

    fn canonical_struct(
        &self,
        item: &ItemStruct,
        scope: &str,
        subst: &TypeSubst,
        stack: &mut Vec<(String, String)>,
    ) -> syn::Result<String> {
        match &item.fields {
            Fields::Named(fields) => {
                let attrs = SerdeAttrs::parse(&item.attrs, SerdeAttrLocation::StructContainer)?;
                let rename_all = attrs.rename_all.as_ref();
                self.canonical_named_fields(
                    fields,
                    rename_all,
                    SerdeAttrLocation::StructField,
                    scope,
                    subst,
                    stack,
                )
            }
            Fields::Unnamed(fields) => {
                SerdeAttrs::parse(&item.attrs, SerdeAttrLocation::TupleStructContainer)?;
                if fields.unnamed.len() == 1 {
                    let field = fields.unnamed.iter().next().expect("checked len");
                    let ty = self.canonical_field_type(
                        field,
                        SerdeAttrLocation::NewtypeField,
                        scope,
                        subst,
                        stack,
                    )?;
                    Ok(format!("newtype({ty})"))
                } else {
                    self.canonical_unnamed_fields(
                        fields,
                        SerdeAttrLocation::TupleField,
                        scope,
                        subst,
                        stack,
                    )
                }
            }
            Fields::Unit => {
                SerdeAttrs::parse(&item.attrs, SerdeAttrLocation::UnitStructContainer)?;
                Ok("unit".to_string())
            }
        }
    }

    fn canonical_enum(
        &self,
        item: &ItemEnum,
        scope: &str,
        subst: &TypeSubst,
        stack: &mut Vec<(String, String)>,
    ) -> syn::Result<String> {
        let attrs = SerdeAttrs::parse(&item.attrs, SerdeAttrLocation::EnumContainer)?;
        let repr = enum_repr(&attrs, item)?;
        let mut out = format!("enum({repr}){{");
        for variant in &item.variants {
            let variant_location = match &variant.fields {
                Fields::Named(_) => SerdeAttrLocation::EnumStructVariant,
                _ => SerdeAttrLocation::EnumVariant,
            };
            let variant_attrs = SerdeAttrs::parse(&variant.attrs, variant_location)?;
            let wire_name = renamed_variant(
                &variant.ident.to_string(),
                variant_attrs.rename.as_deref(),
                attrs.rename_all.as_ref(),
            );
            let payload = match &variant.fields {
                Fields::Named(fields) => {
                    let rename_all = variant_attrs
                        .rename_all
                        .as_ref()
                        .or(attrs.rename_all_fields.as_ref());
                    self.canonical_named_fields(
                        fields,
                        rename_all,
                        SerdeAttrLocation::EnumVariantField,
                        scope,
                        subst,
                        stack,
                    )?
                }
                Fields::Unnamed(fields) => {
                    if fields.unnamed.len() == 1 {
                        let field = fields.unnamed.iter().next().expect("checked len");
                        let ty = self.canonical_field_type(
                            field,
                            SerdeAttrLocation::EnumVariantNewtypeField,
                            scope,
                            subst,
                            stack,
                        )?;
                        format!("newtype({ty})")
                    } else {
                        self.canonical_unnamed_fields(
                            fields,
                            SerdeAttrLocation::EnumVariantTupleField,
                            scope,
                            subst,
                            stack,
                        )?
                    }
                }
                Fields::Unit => "unit".to_string(),
            };
            out.push_str(&name_token('v', &wire_name));
            out.push('=');
            out.push_str(&payload);
            out.push(';');
        }
        out.push('}');
        Ok(out)
    }

    fn canonical_named_fields(
        &self,
        fields: &syn::FieldsNamed,
        rename_all: Option<&RenameRule>,
        field_location: SerdeAttrLocation,
        scope: &str,
        subst: &TypeSubst,
        stack: &mut Vec<(String, String)>,
    ) -> syn::Result<String> {
        let mut out = String::from("struct{");
        for field in &fields.named {
            let attrs = SerdeAttrs::parse(&field.attrs, field_location)?;
            let ident = field.ident.as_ref().ok_or_else(|| {
                syn::Error::new_spanned(field, "named field is missing an identifier")
            })?;
            let wire_name = renamed_field(&ident.to_string(), attrs.rename.as_deref(), rename_all);
            let ty = self.canonical_field_type_with_attrs(field, &attrs, scope, subst, stack)?;
            out.push_str(&name_token('f', &wire_name));
            out.push('=');
            out.push_str(&ty);
            out.push(';');
        }
        out.push('}');
        Ok(out)
    }

    fn canonical_unnamed_fields(
        &self,
        fields: &syn::FieldsUnnamed,
        field_location: SerdeAttrLocation,
        scope: &str,
        subst: &TypeSubst,
        stack: &mut Vec<(String, String)>,
    ) -> syn::Result<String> {
        let mut out = String::from("tuple(");
        for field in &fields.unnamed {
            let ty = self.canonical_field_type(field, field_location, scope, subst, stack)?;
            out.push_str(&ty);
            out.push(';');
        }
        out.push(')');
        Ok(out)
    }

    fn canonical_field_type(
        &self,
        field: &syn::Field,
        location: SerdeAttrLocation,
        scope: &str,
        subst: &TypeSubst,
        stack: &mut Vec<(String, String)>,
    ) -> syn::Result<String> {
        let attrs = SerdeAttrs::parse(&field.attrs, location)?;
        self.canonical_field_type_with_attrs(field, &attrs, scope, subst, stack)
    }

    fn canonical_field_type_with_attrs(
        &self,
        field: &syn::Field,
        attrs: &SerdeAttrs,
        scope: &str,
        subst: &TypeSubst,
        stack: &mut Vec<(String, String)>,
    ) -> syn::Result<String> {
        if attrs.bytes {
            let inner = self.canonical_type(&field.ty, scope, subst, stack)?;
            if inner == "seq(u8)" {
                return Ok("bytes".to_string());
            }
            return Err(syn::Error::new_spanned(
                field,
                "serde(with = \"serde_bytes\") is only supported on Vec<u8> fields for schema_id",
            ));
        }
        self.canonical_type(&field.ty, scope, subst, stack)
    }

    fn canonical_type(
        &self,
        ty: &Type,
        scope: &str,
        subst: &TypeSubst,
        stack: &mut Vec<(String, String)>,
    ) -> syn::Result<String> {
        match ty {
            Type::Array(array) => {
                let len = array_len(&array.len)?;
                let elem = self.canonical_type(&array.elem, scope, subst, stack)?;
                Ok(format!("array[{len}]({elem})"))
            }
            Type::Path(path) => self.canonical_path_type(path, scope, subst, stack),
            Type::Reference(reference) => self.canonical_type(&reference.elem, scope, subst, stack),
            Type::Slice(slice) => {
                let elem = self.canonical_type(&slice.elem, scope, subst, stack)?;
                Ok(format!("seq({elem})"))
            }
            Type::Tuple(tuple) => {
                if tuple.elems.is_empty() {
                    return Ok("nil".to_string());
                }
                let mut out = String::from("tuple(");
                for elem in &tuple.elems {
                    let elem = self.canonical_type(elem, scope, subst, stack)?;
                    out.push_str(&elem);
                    out.push(';');
                }
                out.push(')');
                Ok(out)
            }
            _ => Err(syn::Error::new_spanned(
                ty,
                "type is not part of the schema_id grammar",
            )),
        }
    }

    fn canonical_path_type(
        &self,
        ty: &syn::TypePath,
        scope: &str,
        subst: &TypeSubst,
        stack: &mut Vec<(String, String)>,
    ) -> syn::Result<String> {
        if ty.qself.is_some() {
            return Err(syn::Error::new_spanned(
                ty,
                "qualified self types are not part of the schema_id grammar",
            ));
        }
        let Some(segment) = ty.path.segments.last() else {
            return Err(syn::Error::new_spanned(
                ty,
                "empty type path is not part of the schema_id grammar",
            ));
        };
        let ident = &segment.ident;
        let ident_s = ident.to_string();

        if is_bare_type_path(&ty.path) {
            if let Some(replacement) = subst.get(&ident_s) {
                return self.canonical_type(replacement, scope, subst, stack);
            }

            if self
                .types
                .contains_key(&(scope.to_string(), ident_s.clone()))
            {
                let args = path_type_args(segment, ty)?;
                return self.canonical_named(scope, ident, &args, subst, stack);
            }
        }

        match modeled_type_path(&ty.path) {
            Some(ModeledTypePath::Scalar(name)) => {
                ensure_no_generic_args(segment, ty)?;
                Ok(name.to_string())
            }
            Some(ModeledTypePath::String) => {
                ensure_no_generic_args(segment, ty)?;
                Ok("string".to_string())
            }
            Some(ModeledTypePath::Option) => {
                let args = generic_args(segment, 1, ty)?;
                let inner = self.canonical_type(&args[0], scope, subst, stack)?;
                Ok(format!("option({inner})"))
            }
            Some(ModeledTypePath::Seq) => {
                let args = generic_args(segment, 1, ty)?;
                let inner = self.canonical_type(&args[0], scope, subst, stack)?;
                Ok(format!("seq({inner})"))
            }
            Some(ModeledTypePath::Map) => {
                let args = generic_args(segment, 2, ty)?;
                let key = self.canonical_type(&args[0], scope, subst, stack)?;
                let value = self.canonical_type(&args[1], scope, subst, stack)?;
                Ok(format!("map({key},{value})"))
            }
            None if ident_s == "usize" || ident_s == "isize" => Err(syn::Error::new_spanned(
                ty,
                "usize/isize are not allowed in schema_id because their wire width is not explicit",
            )),
            None => Err(unmodeled_type_error(ty, scope)),
        }
    }
}

#[derive(Clone, Copy)]
enum ModeledTypePath {
    Scalar(&'static str),
    String,
    Option,
    Seq,
    Map,
}

fn modeled_type_path(path: &Path) -> Option<ModeledTypePath> {
    for scalar in [
        "bool", "str", "u8", "u16", "u32", "u64", "u128", "i8", "i16", "i32", "i64", "i128", "f32",
        "f64",
    ] {
        if path_is_bare_name(path, scalar)
            || path_is(path, &["std", "primitive", scalar])
            || path_is(path, &["core", "primitive", scalar])
        {
            return Some(ModeledTypePath::Scalar(scalar));
        }
    }

    if path_is_bare_name(path, "String")
        || path_is(path, &["std", "string", "String"])
        || path_is(path, &["alloc", "string", "String"])
    {
        return Some(ModeledTypePath::String);
    }

    if path_is_bare_name(path, "Option")
        || path_is(path, &["std", "option", "Option"])
        || path_is(path, &["core", "option", "Option"])
    {
        return Some(ModeledTypePath::Option);
    }

    if path_is_bare_name(path, "Vec")
        || path_is(path, &["std", "vec", "Vec"])
        || path_is(path, &["alloc", "vec", "Vec"])
    {
        return Some(ModeledTypePath::Seq);
    }

    if path_is_bare_name(path, "HashMap") || path_is(path, &["std", "collections", "HashMap"]) {
        return Some(ModeledTypePath::Map);
    }

    if path_is_bare_name(path, "BTreeMap")
        || path_is(path, &["std", "collections", "BTreeMap"])
        || path_is(path, &["alloc", "collections", "BTreeMap"])
    {
        return Some(ModeledTypePath::Map);
    }

    None
}

fn is_bare_type_path(path: &Path) -> bool {
    path.leading_colon.is_none() && path.segments.len() == 1
}

fn path_is_bare_name(path: &Path, name: &str) -> bool {
    is_bare_type_path(path) && path.segments[0].ident == name
}

fn path_is(path: &Path, names: &[&str]) -> bool {
    // A `std`/`core`/`alloc`-qualified type denotes the real standard type only when written
    // ABSOLUTELY (`::std::...`). A relative `std::...` path can resolve to a local API module of
    // the same name, so it must NOT match a modeled built-in - it falls through to local
    // resolution (and is rejected fail-closed if it is not a local type). Authors qualify with a
    // leading `::` or use the bare name.
    path.leading_colon.is_some()
        && path.segments.len() == names.len()
        && path
            .segments
            .iter()
            .zip(names)
            .enumerate()
            .all(|(idx, (segment, name))| {
                segment.ident == *name
                    && (idx + 1 == names.len() || matches!(segment.arguments, PathArguments::None))
            })
}

fn ensure_no_generic_args(segment: &syn::PathSegment, span: &TypePath) -> syn::Result<()> {
    let args = path_type_args(segment, span)?;
    if args.is_empty() {
        Ok(())
    } else {
        Err(syn::Error::new_spanned(
            span,
            format!(
                "{} expects 0 generic argument(s), got {}",
                segment.ident,
                args.len()
            ),
        ))
    }
}

fn unmodeled_type_error(ty: &TypePath, scope: &str) -> syn::Error {
    syn::Error::new_spanned(
        ty,
        format!(
            "type `{}` is not part of the schema_id type allowlist for API node `{scope}`; the canonicalizer does not model its wire shape",
            quote!(#ty)
        ),
    )
}

type TypeSubst = std::collections::HashMap<String, Type>;

fn substitute_type(ty: &Type, subst: &TypeSubst) -> Type {
    substitute_type_inner(ty, subst, &mut Vec::new())
}

fn substitute_type_inner(ty: &Type, subst: &TypeSubst, seen: &mut Vec<String>) -> Type {
    match ty {
        Type::Array(array) => {
            let mut array = array.clone();
            array.elem = Box::new(substitute_type_inner(&array.elem, subst, seen));
            Type::Array(array)
        }
        Type::Path(path) => substitute_path_type(path, subst, seen),
        Type::Reference(reference) => {
            let mut reference = reference.clone();
            reference.elem = Box::new(substitute_type_inner(&reference.elem, subst, seen));
            Type::Reference(reference)
        }
        Type::Slice(slice) => {
            let mut slice = slice.clone();
            slice.elem = Box::new(substitute_type_inner(&slice.elem, subst, seen));
            Type::Slice(slice)
        }
        Type::Tuple(tuple) => {
            let mut tuple = tuple.clone();
            tuple.elems = tuple
                .elems
                .into_iter()
                .map(|elem| substitute_type_inner(&elem, subst, seen))
                .collect();
            Type::Tuple(tuple)
        }
        _ => ty.clone(),
    }
}

fn substitute_path_type(path: &TypePath, subst: &TypeSubst, seen: &mut Vec<String>) -> Type {
    if path.qself.is_none()
        && path.path.segments.len() == 1
        && let Some(segment) = path.path.segments.first()
    {
        let ident = segment.ident.to_string();
        if !seen.contains(&ident)
            && let Some(replacement) = subst.get(&ident)
        {
            seen.push(ident);
            let replacement = substitute_type_inner(replacement, subst, seen);
            seen.pop();
            return replacement;
        }
    }

    let mut path = path.clone();
    for segment in &mut path.path.segments {
        if let PathArguments::AngleBracketed(args) = &mut segment.arguments {
            for arg in &mut args.args {
                if let GenericArgument::Type(ty) = arg {
                    *ty = substitute_type_inner(ty, subst, seen);
                }
            }
        }
    }
    Type::Path(path)
}

fn schema_hash(canonical: &str) -> String {
    let digest = Sha256::digest(canonical.as_bytes());
    let mut full = String::with_capacity(64);
    for byte in digest {
        full.push_str(&format!("{byte:02x}"));
    }
    full.truncate(16);
    full
}

fn type_params(ty: &TypeDef) -> syn::Result<Vec<String>> {
    let generics = match ty {
        TypeDef::Struct(item) => &item.generics,
        TypeDef::Enum(item) => &item.generics,
    };
    let mut params = Vec::new();
    for param in &generics.params {
        match param {
            GenericParam::Type(type_param) => params.push(type_param.ident.to_string()),
            GenericParam::Lifetime(param) => {
                return Err(syn::Error::new_spanned(
                    param,
                    "lifetime generics are not part of the schema_id grammar",
                ));
            }
            GenericParam::Const(param) => {
                return Err(syn::Error::new_spanned(
                    param,
                    "const generics are not part of the schema_id grammar",
                ));
            }
        }
    }
    Ok(params)
}

fn generic_args(
    segment: &syn::PathSegment,
    expected: usize,
    span: &TypePath,
) -> syn::Result<Vec<Type>> {
    let args = path_type_args(segment, span)?;
    if args.len() != expected {
        return Err(syn::Error::new_spanned(
            span,
            format!(
                "{} expects {expected} generic argument(s), got {}",
                segment.ident,
                args.len()
            ),
        ));
    }
    Ok(args)
}

fn path_type_args(segment: &syn::PathSegment, span: &TypePath) -> syn::Result<Vec<Type>> {
    match &segment.arguments {
        PathArguments::None => Ok(Vec::new()),
        PathArguments::AngleBracketed(args) => {
            let mut types = Vec::new();
            for arg in &args.args {
                match arg {
                    GenericArgument::Type(ty) => types.push(ty.clone()),
                    _ => {
                        return Err(syn::Error::new_spanned(
                            arg,
                            "only type generic arguments are part of the schema_id grammar",
                        ));
                    }
                }
            }
            Ok(types)
        }
        PathArguments::Parenthesized(_) => Err(syn::Error::new_spanned(
            span,
            "function-style generic arguments are not part of the schema_id grammar",
        )),
    }
}

fn array_len(expr: &Expr) -> syn::Result<String> {
    match expr {
        Expr::Lit(expr_lit) => match &expr_lit.lit {
            Lit::Int(lit) => Ok(lit.base10_digits().to_string()),
            _ => Err(syn::Error::new_spanned(
                expr,
                "array length in schema_id must be an integer literal",
            )),
        },
        _ => Err(syn::Error::new_spanned(
            expr,
            "array length in schema_id must be an integer literal",
        )),
    }
}

fn enum_repr(attrs: &SerdeAttrs, item: &ItemEnum) -> syn::Result<String> {
    if attrs.untagged {
        if attrs.tag.is_some() || attrs.content.is_some() {
            return Err(syn::Error::new_spanned(
                item,
                "serde(untagged) cannot be combined with tag/content for schema_id",
            ));
        }
        return Ok("untagged".to_string());
    }
    match (attrs.tag.as_deref(), attrs.content.as_deref()) {
        (Some(tag), Some(content)) => Ok(format!(
            "adjacent(tag={},content={})",
            raw_name(tag),
            raw_name(content)
        )),
        (Some(tag), None) => Ok(format!("internal(tag={})", raw_name(tag))),
        (None, Some(_)) => Err(syn::Error::new_spanned(
            item,
            "serde(content = ...) without serde(tag = ...) is not part of the schema_id grammar",
        )),
        (None, None) => Ok("external".to_string()),
    }
}

#[derive(Clone)]
struct SerdeAttrs {
    rename: Option<String>,
    rename_all: Option<RenameRule>,
    rename_all_fields: Option<RenameRule>,
    tag: Option<String>,
    content: Option<String>,
    untagged: bool,
    bytes: bool,
}

#[derive(Clone, Copy)]
enum SerdeAttrLocation {
    StructContainer,
    TupleStructContainer,
    UnitStructContainer,
    EnumContainer,
    EnumVariant,
    EnumStructVariant,
    StructField,
    NewtypeField,
    TupleField,
    EnumVariantField,
    EnumVariantNewtypeField,
    EnumVariantTupleField,
}

impl SerdeAttrLocation {
    fn label(self) -> &'static str {
        match self {
            SerdeAttrLocation::StructContainer => "struct container",
            SerdeAttrLocation::TupleStructContainer => "tuple struct container",
            SerdeAttrLocation::UnitStructContainer => "unit struct container",
            SerdeAttrLocation::EnumContainer => "enum container",
            SerdeAttrLocation::EnumVariant => "enum variant",
            SerdeAttrLocation::EnumStructVariant => "enum variant",
            SerdeAttrLocation::StructField => "struct field",
            SerdeAttrLocation::NewtypeField => "newtype field",
            SerdeAttrLocation::TupleField => "tuple field",
            SerdeAttrLocation::EnumVariantField => "enum variant field",
            SerdeAttrLocation::EnumVariantNewtypeField => "enum variant newtype field",
            SerdeAttrLocation::EnumVariantTupleField => "enum variant tuple field",
        }
    }

    fn allows_rename(self) -> bool {
        matches!(
            self,
            SerdeAttrLocation::EnumVariant
                | SerdeAttrLocation::EnumStructVariant
                | SerdeAttrLocation::StructField
                | SerdeAttrLocation::EnumVariantField
        )
    }

    fn allows_rename_all(self) -> bool {
        matches!(
            self,
            SerdeAttrLocation::StructContainer
                | SerdeAttrLocation::EnumContainer
                | SerdeAttrLocation::EnumStructVariant
        )
    }

    fn allows_rename_all_fields(self) -> bool {
        matches!(self, SerdeAttrLocation::EnumContainer)
    }

    fn allows_enum_repr(self) -> bool {
        matches!(self, SerdeAttrLocation::EnumContainer)
    }

    fn allows_bytes(self) -> bool {
        matches!(
            self,
            SerdeAttrLocation::StructField
                | SerdeAttrLocation::NewtypeField
                | SerdeAttrLocation::TupleField
                | SerdeAttrLocation::EnumVariantField
                | SerdeAttrLocation::EnumVariantNewtypeField
                | SerdeAttrLocation::EnumVariantTupleField
        )
    }
}

impl SerdeAttrs {
    fn parse(attrs: &[Attribute], location: SerdeAttrLocation) -> syn::Result<Self> {
        let mut parsed = SerdeAttrs {
            rename: None,
            rename_all: None,
            rename_all_fields: None,
            tag: None,
            content: None,
            untagged: false,
            bytes: false,
        };
        for attr in attrs {
            if !attr.path().is_ident("serde") {
                validate_neutral_attr(attr)?;
                continue;
            }
            attr.parse_nested_meta(|meta| {
                if meta.path.is_ident("rename") {
                    reject_unless_allowed(&meta, location, location.allows_rename())?;
                    parsed.rename = Some(parse_lit_str(&meta, "rename")?);
                    return Ok(());
                }
                if meta.path.is_ident("rename_all") {
                    reject_unless_allowed(&meta, location, location.allows_rename_all())?;
                    let value = parse_lit_str(&meta, "rename_all")?;
                    parsed.rename_all = Some(RenameRule::parse(&value, meta.path.span())?);
                    return Ok(());
                }
                if meta.path.is_ident("rename_all_fields") {
                    reject_unless_allowed(&meta, location, location.allows_rename_all_fields())?;
                    let value = parse_lit_str(&meta, "rename_all_fields")?;
                    parsed.rename_all_fields = Some(RenameRule::parse(&value, meta.path.span())?);
                    return Ok(());
                }
                if meta.path.is_ident("tag") {
                    reject_unless_allowed(&meta, location, location.allows_enum_repr())?;
                    parsed.tag = Some(parse_lit_str(&meta, "tag")?);
                    return Ok(());
                }
                if meta.path.is_ident("content") {
                    reject_unless_allowed(&meta, location, location.allows_enum_repr())?;
                    parsed.content = Some(parse_lit_str(&meta, "content")?);
                    return Ok(());
                }
                if meta.path.is_ident("untagged") {
                    reject_unless_allowed(&meta, location, location.allows_enum_repr())?;
                    parsed.untagged = true;
                    return Ok(());
                }
                if meta.path.is_ident("with") {
                    let value = parse_lit_str(&meta, "with")?;
                    if location.allows_bytes() && is_serde_bytes_path(&value) {
                        parsed.bytes = true;
                        return Ok(());
                    }
                    if location.allows_bytes() {
                        return Err(meta.error(format!(
                            "serde(with = {value:?}) is not allowed on {} for schema_id; only serde_bytes is canonicalized as bytes",
                            location.label()
                        )));
                    }
                    return Err(serde_attr_not_allowed(&meta, location));
                }
                Err(serde_attr_not_allowed(&meta, location))
            })?;
        }
        Ok(parsed)
    }
}

fn reject_unless_allowed(
    meta: &syn::meta::ParseNestedMeta<'_>,
    location: SerdeAttrLocation,
    allowed: bool,
) -> syn::Result<()> {
    if allowed {
        Ok(())
    } else {
        Err(serde_attr_not_allowed(meta, location))
    }
}

fn serde_attr_not_allowed(
    meta: &syn::meta::ParseNestedMeta<'_>,
    location: SerdeAttrLocation,
) -> syn::Error {
    meta.error(format!(
        "serde({}) is not allowed on {} for schema_id; the canonicalizer does not model its wire effect",
        path_name(&meta.path),
        location.label()
    ))
}

fn is_serde_bytes_path(value: &str) -> bool {
    value == "serde_bytes" || value == "::serde_bytes"
}

fn validate_neutral_attr(attr: &Attribute) -> syn::Result<()> {
    if attr.path().is_ident("doc")
        || attr.path().is_ident("allow")
        || attr.path().is_ident("expect")
    {
        return Ok(());
    }

    if attr.path().is_ident("derive") {
        let derives = attr.parse_args_with(Punctuated::<Path, Token![,]>::parse_terminated)?;
        for derive in derives {
            if !is_wire_neutral_derive(&derive) {
                return Err(syn::Error::new_spanned(
                    derive,
                    "derive is not known wire-neutral for schema_id",
                ));
            }
        }
        return Ok(());
    }

    Err(syn::Error::new_spanned(
        attr,
        format!(
            "unsupported outer attribute `{}` for schema_id; only doc, derive, allow, expect, and serde attributes are supported",
            path_name(attr.path())
        ),
    ))
}

fn is_wire_neutral_derive(path: &Path) -> bool {
    let Some(segment) = path.segments.last() else {
        return false;
    };
    if !matches!(segment.arguments, PathArguments::None) {
        return false;
    }
    matches!(
        segment.ident.to_string().as_str(),
        "Copy" | "Clone" | "Debug" | "PartialEq" | "Eq" | "PartialOrd" | "Ord" | "Hash" | "Default"
    )
}

fn path_name(path: &Path) -> String {
    path.segments
        .iter()
        .map(|segment| segment.ident.to_string())
        .collect::<Vec<_>>()
        .join("::")
}

fn parse_lit_str(meta: &syn::meta::ParseNestedMeta<'_>, name: &str) -> syn::Result<String> {
    let value = meta.value().map_err(|_| {
        meta.error(format!(
            "serde({name}) must be a single string literal for schema_id"
        ))
    })?;
    let lit: LitStr = value.parse()?;
    Ok(lit.value())
}

#[derive(Clone, Debug)]
enum RenameRule {
    Lower,
    Upper,
    Pascal,
    Camel,
    Snake,
    ScreamingSnake,
    Kebab,
    ScreamingKebab,
}

impl RenameRule {
    fn parse(value: &str, span: proc_macro2::Span) -> syn::Result<Self> {
        match value {
            "lowercase" => Ok(RenameRule::Lower),
            "UPPERCASE" => Ok(RenameRule::Upper),
            "PascalCase" => Ok(RenameRule::Pascal),
            "camelCase" => Ok(RenameRule::Camel),
            "snake_case" => Ok(RenameRule::Snake),
            "SCREAMING_SNAKE_CASE" => Ok(RenameRule::ScreamingSnake),
            "kebab-case" => Ok(RenameRule::Kebab),
            "SCREAMING-KEBAB-CASE" => Ok(RenameRule::ScreamingKebab),
            _ => Err(syn::Error::new(
                span,
                format!("unsupported serde rename_all rule `{value}` for schema_id"),
            )),
        }
    }

    fn apply_to_variant(&self, name: &str) -> String {
        match self {
            RenameRule::Lower => name.to_ascii_lowercase(),
            RenameRule::Upper => name.to_ascii_uppercase(),
            RenameRule::Pascal => name.to_string(),
            RenameRule::Camel => lower_first(name),
            RenameRule::Snake => {
                let mut snake = String::new();
                for (i, ch) in name.char_indices() {
                    if i > 0 && ch.is_uppercase() {
                        snake.push('_');
                    }
                    snake.push(ch.to_ascii_lowercase());
                }
                snake
            }
            RenameRule::ScreamingSnake => self.apply_screaming_snake_variant(name),
            RenameRule::Kebab => self.apply_kebab_variant(name),
            RenameRule::ScreamingKebab => {
                self.apply_screaming_snake_variant(name).replace('_', "-")
            }
        }
    }

    fn apply_to_field(&self, name: &str) -> String {
        match self {
            RenameRule::Lower | RenameRule::Snake => name.to_string(),
            RenameRule::Upper => name.to_ascii_uppercase(),
            RenameRule::Pascal => {
                let mut pascal = String::new();
                let mut capitalize = true;
                for ch in name.chars() {
                    if ch == '_' {
                        capitalize = true;
                    } else if capitalize {
                        pascal.push(ch.to_ascii_uppercase());
                        capitalize = false;
                    } else {
                        pascal.push(ch);
                    }
                }
                pascal
            }
            RenameRule::Camel => lower_first(&RenameRule::Pascal.apply_to_field(name)),
            RenameRule::ScreamingSnake => name.to_ascii_uppercase(),
            RenameRule::Kebab => name.replace('_', "-"),
            RenameRule::ScreamingKebab => name.to_ascii_uppercase().replace('_', "-"),
        }
    }

    fn apply_screaming_snake_variant(&self, name: &str) -> String {
        RenameRule::Snake
            .apply_to_variant(name)
            .to_ascii_uppercase()
    }

    fn apply_kebab_variant(&self, name: &str) -> String {
        RenameRule::Snake.apply_to_variant(name).replace('_', "-")
    }
}

fn lower_first(name: &str) -> String {
    let Some(first) = name.chars().next() else {
        return String::new();
    };
    let rest = &name[first.len_utf8()..];
    let mut lowered = String::new();
    lowered.push(first.to_ascii_lowercase());
    lowered.push_str(rest);
    lowered
}

fn renamed_field(name: &str, rename: Option<&str>, rename_all: Option<&RenameRule>) -> String {
    // serde derives the default wire name from the *unraw* identifier (`r#foo` -> `foo`);
    // an explicit `rename = "..."` is used verbatim (never unrawed).
    let name = unraw(name);
    if let Some(rename) = rename {
        rename.to_string()
    } else if let Some(rule) = rename_all {
        rule.apply_to_field(name)
    } else {
        name.to_string()
    }
}

fn renamed_variant(name: &str, rename: Option<&str>, rename_all: Option<&RenameRule>) -> String {
    let name = unraw(name);
    if let Some(rename) = rename {
        rename.to_string()
    } else if let Some(rule) = rename_all {
        rule.apply_to_variant(name)
    } else {
        name.to_string()
    }
}

/// Strip a raw-identifier prefix (`r#type` -> `type`), matching how serde derives the
/// default wire name. Applied to the base identifier, never to an explicit `rename`.
fn unraw(name: &str) -> &str {
    name.strip_prefix("r#").unwrap_or(name)
}

fn name_token(prefix: char, name: &str) -> String {
    format!("{prefix}{}:{name}", name.len())
}

fn raw_name(name: &str) -> String {
    format!("{}:{name}", name.len())
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
/// `{var}`-placeholder doc key. Both are derived from the node path so the concrete
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
    use super::{ApiTree, ShapeCatalog, TypeSubst, expand};
    use proc_macro2::{Span, TokenStream};
    use quote::quote;
    use syn::Ident;

    fn catalog(input: TokenStream) -> ShapeCatalog {
        let tree: ApiTree = syn::parse2(input).expect("test api tree parses");
        ShapeCatalog::new(&tree.versions[0].nodes).expect("test catalog builds")
    }

    fn canonical_body(catalog: &ShapeCatalog, scope: &str, ident: &str) -> String {
        let ident = Ident::new(ident, Span::call_site());
        let subst = TypeSubst::new();
        let mut stack = Vec::new();
        catalog
            .canonical_named(scope, &ident, &[], &subst, &mut stack)
            .expect("test body canonicalizes")
    }

    fn schema_id(catalog: &ShapeCatalog, scope: &str, ident: &str) -> String {
        let ident = Ident::new(ident, Span::call_site());
        catalog
            .schema_id_for(scope, &ident)
            .expect("test schema id canonicalizes")
    }

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

    #[test]
    fn schema_id_distinguishes_newtype_struct_from_tuple_field() {
        let catalog = catalog(quote! {
            version v {
                wire {
                    struct N(u8);
                    struct HoldsNewtype { value: N }
                    struct HoldsTuple { value: (u8,) }

                    topic newtype: state HoldsNewtype;
                    topic tuple: state HoldsTuple;
                }
            }
        });

        assert_eq!(canonical_body(&catalog, "wire", "N"), "newtype(u8)");
        assert_eq!(
            canonical_body(&catalog, "wire", "HoldsNewtype"),
            "struct{f5:value=newtype(u8);}"
        );
        assert_eq!(
            canonical_body(&catalog, "wire", "HoldsTuple"),
            "struct{f5:value=tuple(u8;);}"
        );
        assert_eq!(
            schema_id(&catalog, "wire", "HoldsNewtype"),
            "a3bd530f6ec84c7c"
        );
        assert_eq!(
            schema_id(&catalog, "wire", "HoldsTuple"),
            "ec10e0df68cc072e"
        );
        assert_ne!(
            schema_id(&catalog, "wire", "HoldsNewtype"),
            schema_id(&catalog, "wire", "HoldsTuple")
        );
    }

    #[test]
    fn schema_id_unraws_raw_identifier_field_names() {
        let catalog = catalog(quote! {
            version v {
                wire {
                    struct Raw { r#type: u8 }
                    struct Renamed {
                        #[serde(rename = "r#type")]
                        kind: u8
                    }

                    topic raw: state Raw;
                    topic renamed: state Renamed;
                }
            }
        });

        // serde derives the default wire key from the unraw'd identifier (`type`);
        // an explicit rename to "r#type" stays verbatim - so the two must NOT collide.
        assert_eq!(
            canonical_body(&catalog, "wire", "Raw"),
            "struct{f4:type=u8;}"
        );
        assert_eq!(
            canonical_body(&catalog, "wire", "Renamed"),
            "struct{f6:r#type=u8;}"
        );
        assert_ne!(
            schema_id(&catalog, "wire", "Raw"),
            schema_id(&catalog, "wire", "Renamed")
        );
    }

    #[test]
    fn schema_id_distinguishes_unit_struct_from_nil_type() {
        let catalog = catalog(quote! {
            version v {
                wire {
                    struct U;
                    struct HoldsUnitStruct { value: U }
                    struct HoldsNil { value: () }

                    topic unit_struct: state HoldsUnitStruct;
                    topic nil: state HoldsNil;
                }
            }
        });

        assert_eq!(canonical_body(&catalog, "wire", "U"), "unit");
        assert_eq!(
            canonical_body(&catalog, "wire", "HoldsUnitStruct"),
            "struct{f5:value=unit;}"
        );
        assert_eq!(
            canonical_body(&catalog, "wire", "HoldsNil"),
            "struct{f5:value=nil;}"
        );
        assert_eq!(
            schema_id(&catalog, "wire", "HoldsUnitStruct"),
            "afadc807186b643b"
        );
        assert_eq!(schema_id(&catalog, "wire", "HoldsNil"), "e606e36865c13c05");
        assert_ne!(
            schema_id(&catalog, "wire", "HoldsUnitStruct"),
            schema_id(&catalog, "wire", "HoldsNil")
        );
    }

    #[test]
    fn schema_id_rejects_unknown_outer_attribute() {
        let input = quote! {
            version v {
                wire {
                    #[serde_as(as = "Vec<(_, _)>")]
                    struct Body { value: u8 }
                    topic body: state Body;
                }
            }
        };
        let err = expand(input).expect_err("unknown wire-affecting attrs must fail closed");
        assert!(
            err.to_string()
                .contains("unsupported outer attribute `serde_as`"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_id_rejects_variant_level_with() {
        let input = quote! {
            version v {
                wire {
                    enum Body {
                        #[serde(with = "custom_variant")]
                        Changed(u8),
                    }

                    topic body: state Body;
                }
            }
        };
        let err = expand(input).expect_err("variant-level serde(with) must fail closed");
        assert!(
            err.to_string()
                .contains("serde(with) is not allowed on enum variant"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_id_rejects_variant_level_untagged() {
        let input = quote! {
            version v {
                wire {
                    enum Body {
                        #[serde(untagged)]
                        Changed(u8),
                    }

                    topic body: state Body;
                }
            }
        };
        let err = expand(input).expect_err("variant-level serde(untagged) must fail closed");
        assert!(
            err.to_string()
                .contains("serde(untagged) is not allowed on enum variant"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_id_rejects_field_level_serialize_with() {
        let input = quote! {
            version v {
                wire {
                    struct Body {
                        #[serde(serialize_with = "custom_field")]
                        value: u8,
                    }

                    topic body: state Body;
                }
            }
        };
        let err = expand(input).expect_err("field-level serde(serialize_with) must fail closed");
        assert!(
            err.to_string()
                .contains("serde(serialize_with) is not allowed on struct field"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_id_distinguishes_container_untagged_enum_repr() {
        let catalog = catalog(quote! {
            version v {
                wire {
                    enum External { A, B }

                    #[serde(untagged)]
                    enum Untagged { A, B }

                    topic external: state External;
                    topic untagged: state Untagged;
                }
            }
        });

        assert_eq!(
            canonical_body(&catalog, "wire", "External"),
            "enum(external){v1:A=unit;v1:B=unit;}"
        );
        assert_eq!(
            canonical_body(&catalog, "wire", "Untagged"),
            "enum(untagged){v1:A=unit;v1:B=unit;}"
        );
        assert_eq!(schema_id(&catalog, "wire", "External"), "d7b8d2a0871cb0bb");
        assert_eq!(schema_id(&catalog, "wire", "Untagged"), "33504d536a8c4e98");
        assert_ne!(
            schema_id(&catalog, "wire", "External"),
            schema_id(&catalog, "wire", "Untagged")
        );
    }

    #[test]
    fn schema_id_canonicalizes_serde_bytes_field_as_bytes() {
        let catalog = catalog(quote! {
            version v {
                wire {
                    struct Body {
                        #[serde(with = "serde_bytes")]
                        data: Vec<u8>,
                    }

                    topic body: state Body;
                }
            }
        });

        assert_eq!(
            canonical_body(&catalog, "wire", "Body"),
            "struct{f4:data=bytes;}"
        );
    }

    #[test]
    fn schema_id_rejects_serde_bytes_unless_canonicalized_as_bytes() {
        let input = quote! {
            version v {
                wire {
                    struct Body {
                        #[serde(with = "serde_bytes")]
                        data: String,
                    }

                    topic body: state Body;
                }
            }
        };
        let err = expand(input).expect_err("serde_bytes on non-bytes field must fail closed");
        assert!(
            err.to_string()
                .contains("serde(with = \"serde_bytes\") is only supported on Vec<u8> fields"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_id_rejects_qualified_std_box_when_local_box_exists() {
        let local_catalog = catalog(quote! {
            version v {
                wire {
                    struct Box<T> { value: T }
                    struct Local { b: Box<u8> }

                    topic local: state Local;
                }
            }
        });

        assert_eq!(
            canonical_body(&local_catalog, "wire", "Local"),
            "struct{f1:b=struct{f5:value=u8;};}"
        );

        let input = quote! {
            version v {
                wire {
                    struct Box<T> { value: T }
                    struct Local { b: Box<u8> }
                    struct Std { b: ::std::boxed::Box<u8> }

                    topic local: state Local;
                    topic std: state Std;
                }
            }
        };
        let err = expand(input).expect_err("external std Box must fail closed");
        let err = err.to_string();
        assert!(
            err.contains("std") && err.contains("Box") && err.contains("schema_id type allowlist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_id_rejects_unknown_external_qualified_type() {
        let input = quote! {
            version v {
                wire {
                    struct Body { widget: ::some_crate::Widget }

                    topic body: state Body;
                }
            }
        };
        let err = expand(input).expect_err("unknown external type must fail closed");
        let err = err.to_string();
        assert!(
            err.contains("some_crate")
                && err.contains("Widget")
                && err.contains("schema_id type allowlist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_id_rejects_relative_std_qualified_type() {
        // A relative `std::vec::Vec` is ambiguous: a local API module named `std` could shadow it,
        // so it must NOT be modeled as the real std Vec. Rejected fail-closed - the author writes
        // the bare `Vec` or the absolute `::std::vec::Vec`.
        let input = quote! {
            version v {
                wire {
                    struct Body { v: std::vec::Vec<u8> }

                    topic body: state Body;
                }
            }
        };
        let err = expand(input).expect_err("relative std::vec::Vec must fail closed");
        let err = err.to_string();
        assert!(
            err.contains("schema_id type allowlist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_id_rejects_unknown_bare_single_segment_type() {
        let input = quote! {
            version v {
                wire {
                    struct Body { widget: Widget }

                    topic body: state Body;
                }
            }
        };
        let err = expand(input).expect_err("unknown bare type must fail closed");
        let err = err.to_string();
        assert!(
            err.contains("Widget") && err.contains("schema_id type allowlist"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn schema_id_leading_colon_std_path_does_not_match_local_type() {
        let catalog = catalog(quote! {
            version v {
                wire {
                    struct String { value: u8 }
                    struct Local { s: String }
                    struct Std { s: ::std::string::String }

                    topic local: state Local;
                    topic std: state Std;
                }
            }
        });

        assert_eq!(
            canonical_body(&catalog, "wire", "Local"),
            "struct{f1:s=struct{f5:value=u8;};}"
        );
        assert_eq!(
            canonical_body(&catalog, "wire", "Std"),
            "struct{f1:s=string;}"
        );
        assert_ne!(
            schema_id(&catalog, "wire", "Local"),
            schema_id(&catalog, "wire", "Std")
        );
    }

    #[test]
    fn nested_named_type_does_not_inherit_callers_generic_substitution() {
        let catalog = catalog(quote! {
            version v {
                wire {
                    struct T { value: u8 }
                    struct Inner { value: T }
                    struct Outer<T> { inner: Inner, generic: T }
                    struct UsesU16 { outer: Outer<u16> }

                    topic uses: state UsesU16;
                }
            }
        });

        let body = canonical_body(&catalog, "wire", "UsesU16");
        assert_eq!(
            body,
            "struct{f5:outer=struct{f5:inner=struct{f5:value=struct{f5:value=u8;};};f7:generic=u16;};}"
        );
        assert_eq!(schema_id(&catalog, "wire", "UsesU16"), "88b3f60c330328d9");
    }

    #[test]
    fn rename_all_rules_match_serde_for_tricky_identifiers() {
        let cases = [
            (
                "lowercase",
                "__http_server2",
                "HTTPServer2",
                "__http_server2",
                "httpserver2",
            ),
            (
                "UPPERCASE",
                "__http_server2",
                "HTTPServer2",
                "__HTTP_SERVER2",
                "HTTPSERVER2",
            ),
            (
                "PascalCase",
                "__http_server2",
                "HTTPServer2",
                "HttpServer2",
                "HTTPServer2",
            ),
            (
                "camelCase",
                "__http_server2",
                "HTTPServer2",
                "httpServer2",
                "hTTPServer2",
            ),
            (
                "snake_case",
                "__http_server2",
                "HTTPServer2",
                "__http_server2",
                "h_t_t_p_server2",
            ),
            (
                "SCREAMING_SNAKE_CASE",
                "__http_server2",
                "HTTPServer2",
                "__HTTP_SERVER2",
                "H_T_T_P_SERVER2",
            ),
            (
                "kebab-case",
                "__http_server2",
                "HTTPServer2",
                "--http-server2",
                "h-t-t-p-server2",
            ),
            (
                "SCREAMING-KEBAB-CASE",
                "__http_server2",
                "HTTPServer2",
                "--HTTP-SERVER2",
                "H-T-T-P-SERVER2",
            ),
        ];

        for (rule, field, variant, expected_field, expected_variant) in cases {
            let rule = super::RenameRule::parse(rule, Span::call_site())
                .expect("supported serde rename_all rule parses");
            assert_eq!(rule.apply_to_field(field), expected_field, "{rule:?} field");
            assert_eq!(
                rule.apply_to_variant(variant),
                expected_variant,
                "{rule:?} variant"
            );
        }
    }

    #[test]
    fn canonical_schema_uses_serde_rename_all_wire_names() {
        let catalog = catalog(quote! {
            version v {
                wire {
                    #[serde(rename_all = "camelCase")]
                    struct TrickyField { __http_server2: u8 }

                    #[serde(rename_all = "snake_case")]
                    enum TrickyVariant { HTTPServer2 }

                    topic field: state TrickyField;
                    topic variant: state TrickyVariant;
                }
            }
        });

        assert_eq!(
            canonical_body(&catalog, "wire", "TrickyField"),
            "struct{f11:httpServer2=u8;}"
        );
        assert_eq!(
            canonical_body(&catalog, "wire", "TrickyVariant"),
            "enum(external){v15:h_t_t_p_server2=unit;}"
        );
    }
}
