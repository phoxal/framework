//! `phoxal_api_tree!` - the single API layer (D60/D61/D1).
//!
//! Grammar. The body of a `version` is a tree of **nodes**. A node is either
//! static (`name { … }`) or dynamic (`name(var) { … }`); it can be nested to any
//! depth and may hold any mix of types (`struct`/`enum`), `topic` declarations,
//! and child nodes. Every topic declares a **role**: `topic name: command Body;`
//! (a control input the owner subscribes), `topic name: state Body;` (telemetry
//! the owner publishes), or `topic name: query Request => Response;`
//! (request/response). `command` and `state` are both pub/sub on the wire; the
//! role drives the side-branded builders (L1): the public client builder
//! (`api::topic::client()...`) and the owner builder (`api::topic::owner()...`)
//! return side-branded topics (`Publish`/`Subscribe`/`AskQuery`/`ServeQuery`), so
//! taking the wrong side does not compile. The role is also emitted as a `ROLE`
//! const on each body (D63). A topic's key and dynamism are
//! derived from the node path, not from per-topic params; a topic whose node path
//! contains at least one `(var)` node is dynamic, one with none is static.
//! `topic self: state <Body>;` binds the body to the node path itself instead of
//! appending a leaf segment, for framework infrastructure topics such as
//! `logs/{participant_id}`.
//!
//! `topic <leaf>: world_clock <Body>;` is a fifth, framework-reserved role: it
//! wire-brands and reports `ROLE` exactly like `state` (owner publishes, client
//! subscribes), but the generated body implements `WorldClockContract` instead
//! of `StateContract`, so the ordinary `state_publisher` builder every
//! participant has cannot name it; the only documented builder is
//! `phoxal::SetupContext::world_clock_publisher`,
//! gated on the sealed world-authority surface. There is exactly one production use -
//! `simulation::Clock` - and no reason for a second. This role exists to close
//! the accidental route to minting world time; it is not an absolute seal, and
//! `TimelineAuthority`'s docs state the exact strength of the guarantee
//! described by `TimelineAuthority`'s public contract.
//! A revision may extend exactly one earlier revision. The child is materialized
//! as a complete concrete tree; inherited definitions are regenerated under the
//! child's identity, while `replace` and `remove` make deltas explicit. Exactly
//! one final `latest <revision>;` declaration emits the facade alias.
//!
//! ```text
//! phoxal_api_tree! {
//!     version v0_1 {
//!         drive {                                  // static node
//!             struct Target { linear_x_mps: f32, angular_z_radps: f32 }
//!             topic target: command Target;        // key v0.1/drive/target
//!             struct State { /* … */ }
//!             topic state: state State;            // owner-published telemetry
//!         }
//!         component(instance) {                    // literal "component" + var {instance}
//!             motor(capability) {                  // literal "motor" + var {capability}
//!                 enum Command { Velocity(f32), Torque(f32), Stop }
//!                 topic command: command Command;
//!                 // path   api::v0_1::component::motor::Command
//!                 // key    v0.1/component/{instance}/motor/{capability}/command
//!             }
//!         }
//!     }
//!     version v0_2 extends v0_1 {
//!         battery { struct State { soc: f32 } topic state: state State; }
//!         drive { replace struct Target { linear_x_mps: f64, angular_z_radps: f64 } }
//!     }
//!     latest v0_2;
//! }
//! ```
//!
//! # Protocol mode
//!
//! The second mode declares a **protocol tree**: one flat contract surface with
//! no revision history.
//!
//! ```text
//! phoxal_api_tree! {
//!     protocol supervisor {
//!         connect {
//!             #[serde(tag = "schema")]
//!             enum Hello {
//!                 #[serde(rename = "supervisor.hello/v0")]
//!                 V0 { token: String },
//!             }
//!             topic hello: command Hello;      // key supervisor/connect/hello
//!         }
//!     }
//! }
//! ```
//!
//! Everything below the tree root is identical to API mode - nested static and
//! dynamic `name(var) { … }` nodes, temporal roles, query typing, the two
//! side-branded builder trees, and the same self-contained path scheme. The
//! differences are all at the root:
//!
//! - **No revision axis.** There are no `version` blocks, no `latest`, and no
//!   `extends` / `replace` / `remove`. Pre-1.0 a protocol is edited in place.
//! - **Relative keys.** A protocol key carries no `v0.1/` segment. The leading
//!   segment is the protocol name itself (`supervisor/connect/hello`), which is
//!   the same slot the dotted revision fills in API mode, so a protocol
//!   composes under the host's execution-scoped bus root exactly like a robot
//!   API topic does (`phoxal/<execution-id>/supervisor/…`).
//! - **The developer owns the schema version.** A protocol body is an ordinary
//!   authored `struct`/`enum`; a document that crosses a process boundary is a
//!   serde-tagged enum whose variants are its schema versions. The macro never
//!   infers a breaking change and never mints a version - it does not read the
//!   body's shape at all.
//! - **`Api::ID` is the protocol name.** A protocol still gets the zero-variant
//!   `enum Api {}` marker every `ContractBody` binds to, so a protocol body and
//!   an API body remain non-interchangeable in the type system. Its
//!   `ApiVersion::ID` - and so each body's `ContractBody::VERSION` - is the
//!   protocol name rather than a dotted revision, because the tree identity is
//!   what that slot names; the payload's own schema version lives in the body's
//!   serde tag, where the developer put it.
//!
//! Each `version` becomes a `pub mod vN` carrying a marker `enum Api {}`
//! (`ApiVersion`), a nested `pub mod` per node holding that node's version-local
//! bodies (plain serde types, no `{"v":…}` wrapper - D62) and their
//! `ContractBody` impls, plus an api-local `topic` builder module.
//!
//! **Wire identity is the version-qualified key, not a transitive-shape hash
//! (D1).** The version is folded into `ContractBody::TOPIC`: a contract's
//! identity is its version-qualified name (`v0.1::drive::Target`), and that
//! name is real on the wire because the key carries it too
//! (`v0.1/drive/target`). Two participants interoperate on a contract iff
//! they use the exact same version-qualified name - enforced by the type system
//! (the `Api` bound) and realized on the wire by the key, which makes two
//! differently-versioned contracts physically incapable of colliding.
//! Published concrete revisions are immutable.
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
//! way to learn its own module path, so a hardcoded `::phoxal_api::vN::…`
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
//!   `topic::owner` under the same name, then every builder module along that
//!   node's subtree, client or owner side, any depth, imports it from its own
//!   parent under the uniform local name `__phoxal_type_root`. A leaf's body
//!   reference is then `self::__phoxal_type_root::<rest of the node path>::<Body>`:
//!   the hop count is always exactly one, never counted against the node's
//!   authored depth or which side is being built.
//!
//! The result: no reference anywhere in the generated tree depends on a computed
//! nesting depth, so lifting a node deeper (or invoking `phoxal_api_tree!` from a
//! more deeply nested module, as the `reused_var_name` / `standalone_version`
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
//! - A concrete revision has an explicit wire identity. A renamed path is an
//!   explicit replacement in a new child revision; a `rename` attribute would
//!   hide that contract change rather than simplify it.
//! - D1 keeps identity on one axis: the version-qualified Rust path is the wire key
//!   (`v0.1::drive::Target` <-> `v0.1/drive/target`). A `rename` attribute
//!   would reopen a second axis (Rust name vs. wire name) for exactly the
//!   contracts where the model guarantees they can never need to diverge.
//!
//! If a future revision needs a differently-worded wire segment than its Rust
//! name reads naturally, declare the new path explicitly in that child revision.

use proc_macro2::TokenStream;
use quote::quote;
use syn::parse::{Parse, ParseStream};
use syn::visit_mut::{self, VisitMut};
use syn::{Ident, ItemEnum, ItemStruct, Token};

use crate::util::body_derives;

mod kw {
    syn::custom_keyword!(extends);
    syn::custom_keyword!(latest);
    syn::custom_keyword!(protocol);
    syn::custom_keyword!(remove);
    syn::custom_keyword!(replace);
    syn::custom_keyword!(version);
    syn::custom_keyword!(topic);
    syn::custom_keyword!(command);
    syn::custom_keyword!(state);
    syn::custom_keyword!(measurement);
    syn::custom_keyword!(diagnostic);
    syn::custom_keyword!(query);
    syn::custom_keyword!(world_clock);
}

pub fn expand(input: TokenStream) -> syn::Result<TokenStream> {
    let tree: ApiTree = syn::parse2(input)?;
    tree.expand()
}

/// One `phoxal_api_tree!` invocation, in exactly one of its two modes.
///
/// The modes are disjoint by construction: a robot API tree is a revision
/// history with a selected `latest`, and a protocol tree is a single flat
/// contract surface whose payload versioning the developer owns. Mixing them in
/// one invocation is rejected at parse time.
enum ApiTree {
    /// Robot API mode: one or more `version` revisions plus the `latest`
    /// selection.
    Api {
        versions: Vec<Version>,
        latest: Ident,
    },
    /// Protocol mode: one or more `protocol <name> { … }` trees.
    Protocols(Vec<Protocol>),
}

/// One `protocol <name> { … }` tree.
///
/// A protocol has no revision history and no version segment. Its `name` is
/// both the generated module and the tree's identity, and it is the leading
/// wire-key segment - exactly the slot the dotted revision occupies in API
/// mode.
struct Protocol {
    name: Ident,
    nodes: Vec<Node>,
}

struct Version {
    name: Ident,
    wire_id: String,
    parent: Option<Ident>,
    nodes: Vec<Node>,
    removals: Vec<Removal>,
}

/// One node in the api tree: a `name { … }` (static) or `name(var) { … }`
/// (dynamic) block that may hold types, topics, and nested child nodes.
#[derive(Clone)]
struct Node {
    name: Ident,
    replace: bool,
    /// The dynamic variable bound by this node (`None` for a static node). When
    /// present, the node contributes `name/{var}` to keys and a var-taking builder
    /// method.
    var: Option<Ident>,
    types: Vec<TypeDef>,
    topics: Vec<TopicDef>,
    children: Vec<Node>,
    removals: Vec<Removal>,
}

#[derive(Clone)]
struct TypeDef {
    replace: bool,
    item: TypeItem,
}

#[derive(Clone)]
enum TypeItem {
    Struct(ItemStruct),
    Enum(ItemEnum),
}

#[derive(Clone)]
struct TopicDef {
    replace: bool,
    leaf: TopicLeaf,
    kind: TopicKind,
    /// The semantic and temporal role declared by the topic's role keyword.
    /// `command`, `state`, `measurement`, and `diagnostic` all produce a
    /// [`TopicKind::PubSub`] on the wire, while `query` produces a
    /// [`TopicKind::Query`]. The role selects the SIDE BRAND in the generated
    /// builders (L1): per (role, side) a leaf returns `Publish` / `Subscribe` /
    /// `AskQuery` / `ServeQuery`, so the public (client) and owner builders
    /// builders return different branded topics. It is also emitted as
    /// `ContractBody::ROLE` plus the matching temporal-role marker impl, which
    /// is what fixes the robot time a publisher of the body can express (#952
    /// section D).
    role: TopicRole,
}

#[derive(Clone)]
struct Removal {
    segments: Vec<Ident>,
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

/// The semantic and temporal role of a topic, mirroring `phoxal_bus::TopicRole`.
/// Parsed from the role keyword and threaded into the generated
/// `ContractBody::ROLE` const and temporal-role marker impl.
///
/// `WorldClock` is a macro-internal refinement with no `phoxal_bus::TopicRole`
/// variant of its own: `bus_variant` reports `TopicRole::State` for it exactly
/// like `State`, but `marker_trait` emits the disjoint `WorldClockContract`
/// instead of `StateContract`, which is what makes the world clock reject the
/// ordinary, unrestricted publisher builder at compile time (see
/// `phoxal_bus::contract::WorldClockContract`'s docs).
#[derive(Clone, Copy, PartialEq, Eq)]
enum TopicRole {
    Command,
    State,
    Measurement,
    Diagnostic,
    Query,
    WorldClock,
}

impl TopicRole {
    /// The `phoxal_bus::TopicRole` variant path this role maps to.
    fn bus_variant(self) -> TokenStream {
        match self {
            TopicRole::Command => quote! { ::phoxal_bus::TopicRole::Command },
            TopicRole::State | TopicRole::WorldClock => quote! { ::phoxal_bus::TopicRole::State },
            TopicRole::Measurement => quote! { ::phoxal_bus::TopicRole::Measurement },
            TopicRole::Diagnostic => quote! { ::phoxal_bus::TopicRole::Diagnostic },
            TopicRole::Query => quote! { ::phoxal_bus::TopicRole::Query },
        }
    }

    /// The temporal-role marker trait a body of this role implements. `query`
    /// has none: a request/response leg expresses no robot time and is served
    /// through the runner, not a publisher handle.
    fn marker_trait(self) -> Option<TokenStream> {
        match self {
            TopicRole::Command => Some(quote! { ::phoxal_bus::CommandContract }),
            TopicRole::State => Some(quote! { ::phoxal_bus::StateContract }),
            TopicRole::Measurement => Some(quote! { ::phoxal_bus::MeasurementContract }),
            TopicRole::Diagnostic => Some(quote! { ::phoxal_bus::DiagnosticContract }),
            TopicRole::WorldClock => Some(quote! { ::phoxal_bus::WorldClockContract }),
            TopicRole::Query => None,
        }
    }

    /// Whether the owning participant publishes this role (as opposed to
    /// subscribing it).
    fn owner_publishes(self) -> bool {
        !matches!(self, TopicRole::Command)
    }
}

#[derive(Clone)]
enum TopicKind {
    PubSub(Ident),
    Query { request: Ident, response: Ident },
}

struct ManifestVersion {
    name: String,
    contracts: Vec<ManifestContract>,
}

struct ManifestContract {
    /// Version-qualified contract identity, e.g. `"v0.1::drive::Target"`
    /// (D1: the version is part of the name, not a separate axis).
    family: String,
    /// Version-qualified wire key, e.g. `"v0.1/drive/target"`.
    topic: String,
    /// The declared role, so a check can enumerate every command topic
    /// without parsing names (#952: every command is classified).
    role: TopicRole,
}

impl Parse for ApiTree {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        if input.peek(kw::protocol) {
            let mut protocols = Vec::new();
            while input.peek(kw::protocol) {
                protocols.push(input.parse()?);
            }
            if !input.is_empty() {
                return Err(input.error(
                    "a `protocol` invocation declares only `protocol <name> { … }` trees: it has \
                     no `version` revisions and no `latest` selection",
                ));
            }
            return Ok(ApiTree::Protocols(protocols));
        }
        let mut versions = Vec::new();
        while input.peek(kw::version) {
            versions.push(input.parse()?);
        }
        if versions.is_empty() {
            return Err(input.error(
                "phoxal_api_tree! requires at least one `version` block or one \
                 `protocol <name> { … }` tree",
            ));
        }
        input.parse::<kw::latest>()?;
        let latest = input.parse()?;
        input.parse::<Token![;]>()?;
        if !input.is_empty() {
            return Err(input.error("expected exactly one final `latest <revision>;` declaration"));
        }
        Ok(ApiTree::Api { versions, latest })
    }
}

impl Parse for Protocol {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::protocol>()?;
        let name: Ident = input.parse()?;
        let text = name.to_string();
        let valid = text
            .chars()
            .next()
            .is_some_and(|first| first.is_ascii_lowercase())
            && text
                .chars()
                .all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_');
        if !valid {
            return Err(syn::Error::new(
                name.span(),
                "a protocol name is a lowercase Rust identifier such as `supervisor`; it is both \
                 the generated module and the leading wire-key segment",
            ));
        }
        let body;
        syn::braced!(body in input);
        let mut nodes = Vec::new();
        while !body.is_empty() {
            if body.peek(kw::remove) {
                return Err(body.error(format!("`remove` {PROTOCOL_HAS_NO_DELTAS}")));
            }
            nodes.push(body.parse()?);
        }
        Ok(Protocol { name, nodes })
    }
}

impl Parse for Version {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::version>()?;
        let name: Ident = input.parse()?;
        let name_text = name.to_string();
        let Some(parts) = name_text.strip_prefix('v') else {
            return Err(syn::Error::new(
                name.span(),
                "API revisions use Rust identifiers such as `v0_1` or `v1_0`",
            ));
        };
        let Some((major, minor)) = parts.split_once('_') else {
            return Err(syn::Error::new(
                name.span(),
                "API revisions use two-part Rust identifiers such as `v0_1` or `v1_0`",
            ));
        };
        let valid_part = |part: &str| {
            !part.is_empty()
                && (part == "0" || !part.starts_with('0'))
                && part.bytes().all(|byte| byte.is_ascii_digit())
        };
        if !valid_part(major) || !valid_part(minor) {
            return Err(syn::Error::new(
                name.span(),
                "API revision components must be canonical decimal numbers, e.g. `v0_1`",
            ));
        }
        let wire_id = format!("v{major}.{minor}");
        let parent = if input.peek(kw::extends) {
            input.parse::<kw::extends>()?;
            Some(input.parse()?)
        } else {
            None
        };
        let body;
        syn::braced!(body in input);
        let mut nodes = Vec::new();
        let mut removals = Vec::new();
        while !body.is_empty() {
            if body.peek(kw::remove) {
                removals.push(body.parse()?);
            } else {
                nodes.push(body.parse()?);
            }
        }
        Ok(Version {
            name,
            wire_id,
            parent,
            nodes,
            removals,
        })
    }
}

impl Parse for Node {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        let replace = if input.peek(kw::replace) {
            input.parse::<kw::replace>()?;
            true
        } else {
            false
        };
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
        let mut removals = Vec::new();
        while !body.is_empty() {
            // Leading doc-comments / attributes apply to the next item; `topic`
            // declarations take none.
            let attrs = body.call(syn::Attribute::parse_outer)?;
            let replace_item = if body.peek(kw::replace) {
                body.parse::<kw::replace>()?;
                true
            } else {
                false
            };
            if body.peek(kw::remove) {
                if replace_item {
                    return Err(body.error("`replace remove` is not valid; use `remove <path>;`"));
                }
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a `remove` declaration",
                    ));
                }
                removals.push(body.parse()?);
            } else if body.peek(kw::topic) {
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a `topic` declaration",
                    ));
                }
                let mut topic: TopicDef = body.parse()?;
                topic.replace = replace_item;
                topics.push(topic);
            } else if body.peek(Token![struct]) {
                let mut item: ItemStruct = body.parse()?;
                item.attrs = attrs;
                item.vis = syn::Visibility::Public(syn::token::Pub::default());
                types.push(TypeDef {
                    replace: replace_item,
                    item: TypeItem::Struct(item),
                });
            } else if body.peek(Token![enum]) {
                let mut item: ItemEnum = body.parse()?;
                item.attrs = attrs;
                item.vis = syn::Visibility::Public(syn::token::Pub::default());
                types.push(TypeDef {
                    replace: replace_item,
                    item: TypeItem::Enum(item),
                });
            } else if body.peek(Ident)
                && (body.peek2(syn::token::Paren) || body.peek2(syn::token::Brace))
            {
                // `name(var) { … }` or `name { … }` - a child node.
                if let Some(attr) = attrs.first() {
                    return Err(syn::Error::new_spanned(
                        attr,
                        "attributes are not allowed on a child node declaration",
                    ));
                }
                let mut child: Node = body.parse()?;
                child.replace = replace_item;
                children.push(child);
            } else {
                return Err(body.error(
                    "expected `struct`, `enum`, `topic …;`, or a child node `name { … }` / \
                     `name(var) { … }` inside an API node block",
                ));
            }
        }
        Ok(Node {
            name,
            replace,
            var,
            types,
            topics,
            children,
            removals,
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
        // Every topic declares a role. `command`, `state`, `measurement`, and
        // `diagnostic` carry a single pub/sub body and differ by role; `query`
        // carries request/response. The role rides alongside the kind and
        // selects the side brand in the generated builders (L1): a `command`
        // leaf is `Publish` on the public builder and `Subscribe` on
        // owner builder; every owner-published role is the reverse.
        let (kind, role) = if input.peek(kw::command) {
            input.parse::<kw::command>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::Command)
        } else if input.peek(kw::state) {
            input.parse::<kw::state>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::State)
        } else if input.peek(kw::measurement) {
            input.parse::<kw::measurement>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::Measurement)
        } else if input.peek(kw::diagnostic) {
            input.parse::<kw::diagnostic>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::Diagnostic)
        } else if input.peek(kw::query) {
            input.parse::<kw::query>()?;
            let request: Ident = input.parse()?;
            input.parse::<Token![=>]>()?;
            let response: Ident = input.parse()?;
            (TopicKind::Query { request, response }, TopicRole::Query)
        } else if input.peek(kw::world_clock) {
            // Framework-reserved: see `TopicRole::WorldClock`'s docs. There is
            // exactly one production use (`simulation::Clock` in
            // `phoxal-api/src/lib.rs`) and no reason for a second.
            input.parse::<kw::world_clock>()?;
            let body: Ident = input.parse()?;
            (TopicKind::PubSub(body), TopicRole::WorldClock)
        } else {
            return Err(input.error(
                "expected a topic role: `command <Body>`, `state <Body>`, \
                 `measurement <Body>`, `diagnostic <Body>`, `world_clock <Body>` \
                 (framework-reserved), or `query <Req> => <Resp>`",
            ));
        };
        input.parse::<Token![;]>()?;
        Ok(TopicDef {
            replace: false,
            leaf,
            kind,
            role,
        })
    }
}

impl Parse for Removal {
    fn parse(input: ParseStream) -> syn::Result<Self> {
        input.parse::<kw::remove>()?;
        let mut segments = vec![input.parse()?];
        while input.peek(Token![::]) {
            input.parse::<Token![::]>()?;
            segments.push(input.parse()?);
        }
        input.parse::<Token![;]>()?;
        Ok(Self { segments })
    }
}

/// The diagnostic tail for a `replace`/`remove` inside a revision that has no
/// parent to delta against.
const VERSION_HAS_NO_PARENT: &str =
    "is only valid inside a revision that `extends` another revision";

/// The diagnostic tail for a `replace`/`remove` inside a protocol tree. A
/// protocol has no revision history at all, so there is nothing to delta - the
/// declaration is edited in place.
const PROTOCOL_HAS_NO_DELTAS: &str = "is not valid inside a `protocol` tree: a protocol has no revision history, so edit the \
     declaration in place";

impl ApiTree {
    fn expand(&self) -> syn::Result<TokenStream> {
        match self {
            ApiTree::Api { versions, latest } => Self::expand_api(versions, latest),
            ApiTree::Protocols(protocols) => Self::expand_protocols(protocols),
        }
    }

    fn expand_protocols(protocols: &[Protocol]) -> syn::Result<TokenStream> {
        let mut out = TokenStream::new();
        let mut manifest_trees = Vec::new();
        let mut declared = std::collections::BTreeSet::<String>::new();
        for protocol in protocols {
            let id = protocol.name.to_string();
            if !declared.insert(id.clone()) {
                return Err(syn::Error::new_spanned(
                    &protocol.name,
                    format!("duplicate protocol tree `{id}`"),
                ));
            }
            validate_complete_tree(&protocol.nodes, &[], PROTOCOL_HAS_NO_DELTAS)?;
            manifest_trees.push(ManifestVersion {
                name: id.clone(),
                contracts: contract_manifest_entries(&id, &protocol.nodes)?,
            });
            out.extend(expand_tree(&MaterializedTree {
                module: protocol.name.clone(),
                doc: format!(
                    "Protocol tree `{id}` - relative wire keys, developer-owned schema-tagged \
                     bodies."
                ),
                id,
                nodes: protocol.nodes.clone(),
            })?);
        }
        let manifest = expand_contract_manifest(&manifest_trees);
        Ok(quote! {
            #manifest
            #out
        })
    }

    fn expand_api(versions: &[Version], latest: &Ident) -> syn::Result<TokenStream> {
        let mut out = TokenStream::new();
        let mut manifest_versions = Vec::new();
        let mut materialized = std::collections::BTreeMap::<String, Vec<Node>>::new();
        for version in versions {
            let name = version.name.to_string();
            if materialized.contains_key(&name) {
                return Err(syn::Error::new_spanned(
                    &version.name,
                    format!("duplicate API revision `{}`", version.name),
                ));
            }
            let nodes = if let Some(parent) = &version.parent {
                let parent_name = parent.to_string();
                let base = materialized.get(&parent_name).ok_or_else(|| {
                    syn::Error::new_spanned(
                        parent,
                        "an `extends` parent must be a concrete revision declared earlier",
                    )
                })?;
                apply_version_delta(base, version)?
            } else {
                validate_complete_tree(&version.nodes, &version.removals, VERSION_HAS_NO_PARENT)?;
                version.nodes.clone()
            };
            let concrete = MaterializedTree {
                module: version.name.clone(),
                id: version.wire_id.clone(),
                doc: format!(
                    "Concrete API revision `{}` - version-local wire bodies + topics.",
                    version.wire_id
                ),
                nodes: nodes.clone(),
            };
            manifest_versions.push(ManifestVersion {
                name: version.wire_id.clone(),
                contracts: contract_manifest_entries(&version.wire_id, &nodes)?,
            });
            out.extend(expand_tree(&concrete)?);
            materialized.insert(name, nodes);
        }
        if !materialized.contains_key(&latest.to_string()) {
            return Err(syn::Error::new_spanned(
                latest,
                "`latest` must name a declared concrete API revision",
            ));
        }
        let manifest = expand_contract_manifest(&manifest_versions);
        Ok(quote! {
            #manifest
            #out
            /// The concrete API revision selected by this framework train.
            pub use #latest as latest;
        })
    }
}

/// One fully resolved tree ready to emit, from either mode.
///
/// `id` is the tree's identity AND its leading wire-key segment: the dotted
/// revision (`"v0.1"`) for an API revision, the protocol name (`"supervisor"`)
/// for a protocol. Everything below this point is mode-agnostic - the two modes
/// differ only in how a tree is parsed, validated, and identified, never in how
/// its modules, bodies, or builders are shaped.
struct MaterializedTree {
    module: Ident,
    id: String,
    doc: String,
    nodes: Vec<Node>,
}

/// Reject the delta forms (`replace` / `remove`) anywhere in a tree that has
/// nothing to delta against. `reason` is the tail of the diagnostic, naming why
/// this particular tree has no parent revision.
fn validate_complete_tree(
    nodes: &[Node],
    removals: &[Removal],
    reason: &'static str,
) -> syn::Result<()> {
    if let Some(removal) = removals.first() {
        return Err(syn::Error::new_spanned(
            &removal.segments[0],
            format!("`remove` {reason}"),
        ));
    }
    for node in nodes {
        if node.replace {
            return Err(syn::Error::new_spanned(
                &node.name,
                format!("`replace` {reason}"),
            ));
        }
        if let Some(removal) = node.removals.first() {
            return Err(syn::Error::new_spanned(
                &removal.segments[0],
                format!("`remove` {reason}"),
            ));
        }
        for ty in &node.types {
            if ty.replace {
                return Err(syn::Error::new_spanned(
                    ty.ident(),
                    format!("`replace` {reason}"),
                ));
            }
        }
        for topic in &node.topics {
            if topic.replace {
                return Err(syn::Error::new_spanned(
                    topic.leaf.method_ident(),
                    format!("`replace` {reason}"),
                ));
            }
        }
        validate_complete_tree(&node.children, &[], reason)?;
    }
    Ok(())
}

fn apply_version_delta(base: &[Node], version: &Version) -> syn::Result<Vec<Node>> {
    let mut nodes = base.to_vec();
    let parent = version
        .parent
        .as_ref()
        .expect("version deltas always have an extends parent");
    reroot_inherited_type_paths(&mut nodes, parent, &version.name);
    apply_removals(&mut nodes, &version.removals)?;
    merge_nodes(&mut nodes, &version.nodes)?;
    Ok(nodes)
}

/// Re-root absolute paths authored against the parent revision when its types
/// are materialized into a child. Without this pass, an inherited body such as
/// `struct Page { cursor: crate::v0_1::tool::Cursor }` would keep referring to
/// the parent's `Cursor` even after the child explicitly replaced that type.
fn reroot_inherited_type_paths(nodes: &mut [Node], parent: &Ident, child: &Ident) {
    struct RevisionPathRewriter<'a> {
        parent: &'a Ident,
        child: &'a Ident,
    }

    impl VisitMut for RevisionPathRewriter<'_> {
        fn visit_path_mut(&mut self, path: &mut syn::Path) {
            let mut segments = path.segments.iter_mut();
            if segments
                .next()
                .is_some_and(|segment| segment.ident == "crate")
                && let Some(revision) = segments.next()
                && revision.ident == *self.parent
            {
                revision.ident = self.child.clone();
            }
            visit_mut::visit_path_mut(self, path);
        }
    }

    fn rewrite_nodes(nodes: &mut [Node], rewriter: &mut RevisionPathRewriter<'_>) {
        for node in nodes {
            for ty in &mut node.types {
                match &mut ty.item {
                    TypeItem::Struct(item) => rewriter.visit_item_struct_mut(item),
                    TypeItem::Enum(item) => rewriter.visit_item_enum_mut(item),
                }
            }
            rewrite_nodes(&mut node.children, rewriter);
        }
    }

    rewrite_nodes(nodes, &mut RevisionPathRewriter { parent, child });
}

fn merge_nodes(base: &mut Vec<Node>, deltas: &[Node]) -> syn::Result<()> {
    for delta in deltas {
        let existing = base.iter().position(|node| node.name == delta.name);
        match (existing, delta.replace) {
            (Some(index), true) => {
                let mut replacement = delta.clone();
                replacement.replace = false;
                validate_complete_tree(&[replacement.clone()], &[], VERSION_HAS_NO_PARENT)?;
                base[index] = replacement;
            }
            (Some(index), false) => merge_node(&mut base[index], delta)?,
            (None, true) => {
                return Err(syn::Error::new_spanned(
                    &delta.name,
                    "`replace` target does not exist in the parent revision",
                ));
            }
            (None, false) => {
                validate_complete_tree(std::slice::from_ref(delta), &[], VERSION_HAS_NO_PARENT)?;
                base.push(delta.clone());
            }
        }
    }
    Ok(())
}

fn merge_node(base: &mut Node, delta: &Node) -> syn::Result<()> {
    if base.var.as_ref().map(Ident::to_string) != delta.var.as_ref().map(Ident::to_string) {
        return Err(syn::Error::new_spanned(
            &delta.name,
            "an inherited node must keep the same static/dynamic binding",
        ));
    }
    apply_node_removals(base, &delta.removals)?;
    for delta_type in &delta.types {
        let ident = delta_type.ident();
        let existing = base.types.iter().position(|item| item.ident() == ident);
        match (existing, delta_type.replace) {
            (Some(index), true) => {
                let mut replacement = delta_type.clone();
                replacement.replace = false;
                base.types[index] = replacement;
            }
            (Some(_), false) => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "inherited type already exists; prefix the declaration with `replace`",
                ));
            }
            (None, true) => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "`replace` type target does not exist in the parent revision",
                ));
            }
            (None, false) => base.types.push(delta_type.clone()),
        }
    }
    for delta_topic in &delta.topics {
        let ident = delta_topic.leaf.method_ident();
        let existing = base
            .topics
            .iter()
            .position(|item| item.leaf.method_ident() == ident);
        match (existing, delta_topic.replace) {
            (Some(index), true) => {
                let mut replacement = delta_topic.clone();
                replacement.replace = false;
                base.topics[index] = replacement;
            }
            (Some(_), false) => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "inherited topic already exists; prefix the declaration with `replace`",
                ));
            }
            (None, true) => {
                return Err(syn::Error::new_spanned(
                    ident,
                    "`replace` topic target does not exist in the parent revision",
                ));
            }
            (None, false) => base.topics.push(delta_topic.clone()),
        }
    }
    merge_nodes(&mut base.children, &delta.children)
}

fn apply_removals(nodes: &mut Vec<Node>, removals: &[Removal]) -> syn::Result<()> {
    for removal in removals {
        remove_from_nodes(nodes, &removal.segments)?;
    }
    Ok(())
}

fn remove_from_nodes(nodes: &mut Vec<Node>, path: &[Ident]) -> syn::Result<()> {
    let Some((head, tail)) = path.split_first() else {
        return Ok(());
    };
    let Some(index) = nodes.iter().position(|node| node.name == *head) else {
        return Err(syn::Error::new_spanned(
            head,
            "`remove` path does not exist in the parent revision",
        ));
    };
    if tail.is_empty() {
        nodes.remove(index);
        return Ok(());
    }
    remove_from_node(&mut nodes[index], tail)
}

fn apply_node_removals(node: &mut Node, removals: &[Removal]) -> syn::Result<()> {
    for removal in removals {
        remove_from_node(node, &removal.segments)?;
    }
    Ok(())
}

fn remove_from_node(node: &mut Node, path: &[Ident]) -> syn::Result<()> {
    let Some((head, tail)) = path.split_first() else {
        return Ok(());
    };
    if !tail.is_empty() {
        let Some(child) = node.children.iter_mut().find(|child| child.name == *head) else {
            return Err(syn::Error::new_spanned(
                head,
                "`remove` path does not exist in the parent revision",
            ));
        };
        return remove_from_node(child, tail);
    }
    let type_index = node.types.iter().position(|item| item.ident() == head);
    let topic_index = node
        .topics
        .iter()
        .position(|item| item.leaf.method_ident() == *head);
    let child_index = node.children.iter().position(|item| item.name == *head);
    let matches = usize::from(type_index.is_some())
        + usize::from(topic_index.is_some())
        + usize::from(child_index.is_some());
    if matches != 1 {
        return Err(syn::Error::new_spanned(
            head,
            if matches == 0 {
                "`remove` target does not exist in the parent revision"
            } else {
                "`remove` target is ambiguous; use a uniquely named path"
            },
        ));
    }
    if let Some(index) = type_index {
        node.types.remove(index);
    } else if let Some(index) = topic_index {
        node.topics.remove(index);
    } else if let Some(index) = child_index {
        node.children.remove(index);
    }
    Ok(())
}

impl TypeDef {
    fn ident(&self) -> &Ident {
        match &self.item {
            TypeItem::Struct(item) => &item.ident,
            TypeItem::Enum(item) => &item.ident,
        }
    }
}

fn expand_contract_manifest(versions: &[ManifestVersion]) -> TokenStream {
    let version_entries = versions.iter().map(|version| {
        let name = &version.name;
        let contracts = version.contracts.iter().map(|contract| {
            let family = &contract.family;
            let topic = &contract.topic;
            let role = contract.role.bus_variant();
            quote! {
                ApiContractManifestContract {
                    family: #family,
                    topic: #topic,
                    role: #role,
                }
            }
        });
        quote! {
            ApiContractManifestVersion {
                name: #name,
                contracts: &[#(#contracts),*],
            }
        }
    });

    quote! {
        /// One generated API version in the contract manifest.
        ///
        /// `#[cfg(test)]`-only: this is the tree's own
        /// self-enumeration, which backs `phoxal-api`'s curation tests (every
        /// command topic is deliberately classified, every wire key composes
        /// as intended). It is available only to test builds.
        #[cfg(test)]
        #[doc(hidden)]
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct ApiContractManifestVersion {
            pub name: &'static str,
            pub contracts: &'static [ApiContractManifestContract],
        }

        /// One generated contract in the contract manifest. `family` is the
        /// version-qualified contract identity (D1); `topic` is its
        /// version-qualified wire key. The name itself is the whole identity.
        /// `#[cfg(test)]`-only - see
        /// [`ApiContractManifestVersion`]'s docs.
        #[cfg(test)]
        #[doc(hidden)]
        #[derive(Clone, Copy, Debug, Eq, PartialEq)]
        pub struct ApiContractManifestContract {
            pub family: &'static str,
            pub topic: &'static str,
            pub role: ::phoxal_bus::TopicRole,
        }

        /// The tree's own enumeration of every contract it declares, used by
        /// `phoxal-api`'s curation tests to assert that each command topic is
        /// deliberately classified and each wire key composes as intended.
        /// `#[cfg(test)]`-only for the two consumers in
        /// `phoxal-api/src/tests.rs`.
        #[cfg(test)]
        #[doc(hidden)]
        pub const API_CONTRACT_MANIFEST: &[ApiContractManifestVersion] = &[#(#version_entries),*];
    }
}

fn contract_manifest_entries(tree_id: &str, nodes: &[Node]) -> syn::Result<Vec<ManifestContract>> {
    let mut contracts = Vec::new();
    collect_contract_manifest_entries(tree_id, nodes, "", "", &mut contracts);
    contracts.sort_by(|left, right| {
        left.family
            .cmp(&right.family)
            .then_with(|| left.topic.cmp(&right.topic))
    });
    Ok(contracts)
}

fn collect_contract_manifest_entries(
    tree_id: &str,
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
            let topic_key = format!("{tree_id}/{}", topic_key(&node_key_prefix, &topic.leaf));
            match &topic.kind {
                TopicKind::PubSub(body) => {
                    contracts.push(ManifestContract {
                        family: format!("{tree_id}::{family_path}::{body}"),
                        topic: topic_key,
                        role: topic.role,
                    });
                }
                TopicKind::Query { request, response } => {
                    contracts.push(ManifestContract {
                        family: format!("{tree_id}::{family_path}::{request}"),
                        topic: topic_key.clone(),
                        role: topic.role,
                    });
                    contracts.push(ManifestContract {
                        family: format!("{tree_id}::{family_path}::{response}"),
                        topic: topic_key,
                        role: topic.role,
                    });
                }
            }
        }

        collect_contract_manifest_entries(
            tree_id,
            &node.children,
            &family_path,
            &node_key_prefix,
            contracts,
        );
    }
}

fn expand_tree(tree: &MaterializedTree) -> syn::Result<TokenStream> {
    let mod_name = &tree.module;
    let id = tree.id.clone();
    let nodes = &tree.nodes;

    // Node modules (types + ContractBody impls), recursive. The family prefix
    // (`::`-joined node names) and the key prefix (`/`-joined `name` or
    // `name/{var}` segments) are threaded down the walk. `id` (the tree's own
    // identity) is threaded down too so every emitted `TOPIC` is qualified by
    // it (D1): the revision - or, in protocol mode, the protocol name - is
    // folded into the wire key, so two trees can never collide.
    let mut node_mods = TokenStream::new();
    for node in nodes {
        node_mods.extend(expand_node_module(node, &id, "", "")?);
    }

    let topic_mod = expand_topic_module(&id, nodes)?;

    let module_doc = &tree.doc;

    Ok(quote! {
        #[doc = #module_doc]
        pub mod #mod_name {
            /// Zero-variant marker identifying this tree (D60): the API
            /// revision in `version` mode, the protocol in `protocol` mode.
            #[derive(Clone, Copy, Debug)]
            pub enum Api {}
            impl ::phoxal_bus::ApiVersion for Api {
                const ID: &'static str = #id;
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

/// Emit a `pub mod <name>` for a node under the tree. The module carries the
/// node's types, the `ContractBody` impls for its topics, and - recursively -
/// its child node modules. Variables never appear in the module path (D61).
///
/// `tree_id` is the tree's identity (the dotted revision `"v0.1"`, or a
/// protocol name), threaded down so every emitted `TOPIC` carries it (D1).
/// `family_prefix` is the
/// `::`-joined ancestor node names (empty at the root); `key_prefix` is the
/// `/`-joined ancestor key segments (`name` or `name/{var}`, empty at the root).
/// The node appends its own contribution to each.
fn expand_node_module(
    node: &Node,
    tree_id: &str,
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
        match &ty.item {
            TypeItem::Struct(item) => {
                let item = with_pub_fields_struct(item.clone());
                types.extend(quote! { #derives #item });
            }
            TypeItem::Enum(item) => {
                types.extend(quote! { #derives #item });
            }
        }
    }

    let mut impls = TokenStream::new();
    for topic in &node.topics {
        // The tree-qualified wire key (D1): folding the tree's identity in here
        // is what makes two trees' contracts physically distinct Zenoh keys.
        let key = format!("{tree_id}/{}", topic_key(&node_key_prefix, &topic.leaf));
        let role = topic.role.bus_variant();
        // The tree-qualified type-path name (D1): the tree identity, then the
        // `::`-joined node path (vars excluded - they are topic params, not
        // type-path segments), then the body's own PascalCase leaf. This is the
        // exact same identity `contract_manifest_entries`' `family` computes for
        // the generated manifest, kept in lockstep by construction (both derive
        // it from `family_path`/`tree_id`). `VERSION`/`CONTRACT` are the split
        // form of the same identity: `VERSION`
        // is just `tree_id` (already a plain literal at this point, not spliced
        // per-body), `CONTRACT` is `family_path::body` with the tree identity
        // dropped - `NAME == VERSION + "::" + CONTRACT` by construction.
        let tree_id = tree_id.to_string();
        let name_for = |body: &Ident| format!("{tree_id}::{family_path}::{body}");
        let contract_for = |body: &Ident| format!("{family_path}::{body}");
        match &topic.kind {
            TopicKind::PubSub(body) => {
                let name = name_for(body);
                let contract = contract_for(body);
                // The role rides as an inherent `#[doc(hidden)] pub const ROLE` on
                // the body: additive surface that does not touch `ContractBody`.
                // The side-branded builders enforce owner/client (L1); the
                // temporal-role marker enforces which publisher handle - and so
                // which robot time - this body admits (#952 section D).
                let marker = topic
                    .role
                    .marker_trait()
                    .map(|marker| quote! { impl #marker for #body {} });
                impls.extend(quote! {
                    impl ::phoxal_bus::ContractBody for #body {
                        type Api = self::__PhoxalApiMarker;
                        const NAME: &'static str = #name;
                        const VERSION: &'static str = #tree_id;
                        const CONTRACT: &'static str = #contract;
                        const TOPIC: &'static str = #key;
                        const ROLE: ::phoxal_bus::TopicRole = #role;
                    }
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
                    }
                    impl ::phoxal_bus::ContractBody for #response {
                        type Api = self::__PhoxalApiMarker;
                        const NAME: &'static str = #response_name;
                        const VERSION: &'static str = #tree_id;
                        const CONTRACT: &'static str = #response_contract;
                        const TOPIC: &'static str = #key;
                        const ROLE: ::phoxal_bus::TopicRole = #role;
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
            tree_id,
            &family_path,
            &node_key_prefix,
        )?);
    }

    Ok(quote! {
        pub mod #name {
            //! Version-local bodies for the `#name_str` node.

            // Forward the tree root's `Api` marker down exactly one hop from
            // this node's own parent (the tree module for a top-level node, or
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
/// - [`Side::Client`] - the PUBLIC `topic::client()...` tree. A `command` leaf yields
///   `Topic<Publish<B>>` (the client sends commands), a `state` leaf yields
///   `Topic<Subscribe<B>>` (the client observes state), and a `query` leaf yields
///   `Topic<AskQuery<Req, Resp>>` (the client calls).
/// - [`Side::Owner`] - the `topic::owner()...` tree. The brands flip: `command` -> `Subscribe` (the owner
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
/// The PUBLIC client tree lives directly under `topic` (`topic::client()` + a builder
/// module per node); the OWNER tree lives under `topic::owner`
/// (`topic::owner()` + the same builder modules, one level deeper). Both
/// mirror the node tree and format identical keys - only the leaf brand differs by
/// side. A dynamic node's method takes its variable as `impl Display` and carries
/// it forward; a leaf method formats the final key from the carried vars.
///
/// Self-contained absolute paths (D1): a builder leaf needs to name a body type
/// that lives in the PARALLEL type-tree hanging off the same tree module
/// (`topic::component::motor::Builder` needs `component::motor::Command`). Rather
/// than counting `super::` hops back to the tree root and down again per leaf,
/// `topic` seeds one hidden alias per top-level node
/// (`#[doc(hidden)] pub use super::component as __phoxal_type_root_component;`,
/// a single, always-valid hop since `topic` is a direct child of the tree
/// module) and `owner` re-forwards each of them one hop further. Every builder
/// module under either side then imports its own top-level node's alias - a
/// single hop from its immediate parent - under the uniform local name
/// `__phoxal_type_root`, and deeper builder modules just forward THAT one hop at
/// a time. A leaf reference is then always `self::__phoxal_type_root::…::Body`:
/// no supers count, no dependency on how deep the node was authored.
fn expand_topic_module(tree_id: &str, nodes: &[Node]) -> syn::Result<TokenStream> {
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
        client_builder_mods.extend(expand_builder_module(node, tree_id, &[], Side::Client)?);
        owner_root_methods.extend(node_entry_method(node));
        owner_builder_mods.extend(expand_builder_module(node, tree_id, &[], Side::Owner)?);

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
        /// PUBLIC `topic::client()...` chain is the CLIENT side; the OWNER side is
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
            // paths, D1): seeded here because `topic` is always exactly one hop
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
    tree_id: &str,
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
        let (fmt_str, doc_key) = builder_leaf_key_parts(tree_id, &path, &topic.leaf);
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
        child_mods.extend(expand_builder_module(child, tree_id, &path, side)?);
    }

    // Self-contained absolute path to this top-level node's type-tree (D1): at the
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
    Ok(quote! {
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
    })
}

/// The branded `Kind` type for a builder leaf, side-aware (L1, plan #00).
///
/// The body path is built from `self::__phoxal_type_root` (this top-level node's
/// type-tree alias, forwarded one hop at a time down from `topic`/`topic::owner`;
/// see [`expand_topic_module`] and [`expand_builder_module`]), followed by the node
/// path's segments after the top-level one (which the alias already denotes), then
/// the body ident. This is a fixed-shape reference that never depends on how deep
/// the node was authored or on which side (client/owner) is being built.
///
/// The brand is picked from `(role, side)`:
///
/// - `command`: client publishes (`Publish`), owner subscribes (`Subscribe`).
/// - `state` / `measurement` / `diagnostic`: client subscribes (`Subscribe`),
///   owner publishes (`Publish`).
/// - `query`: client asks (`AskQuery`), owner serves (`ServeQuery`).
fn builder_leaf_kind(topic: &TopicDef, path: &[NodeSeg], side: Side) -> TokenStream {
    // `path[0]` is the top-level node - exactly what `__phoxal_type_root` already
    // aliases - so only the segments AFTER it need to be descended.
    let rest_path: Vec<&Ident> = path[1..].iter().map(|s| &s.name).collect();
    let body_path = |body: &Ident| quote! { self::__phoxal_type_root #(::#rest_path)* :: #body };
    match &topic.kind {
        TopicKind::PubSub(body) => {
            let b = body_path(body);
            // Every pub/sub role shares the wire shape and differs only in
            // which side publishes; the role + side pick the brand. (A `query`
            // role never carries a `PubSub` kind - the parser pairs it with
            // `TopicKind::Query` - and `owner_publishes` treats it like an
            // owner-published role, which is unreachable but harmless.)
            let owner_publishes = topic.role.owner_publishes();
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

/// Build a leaf's key in two forms: the `format!` template (the tree identity
/// as a literal leading segment, then literal node-name segments, `{}` for each
/// dynamic var, optionally then `/leaf`) and the human-readable
/// `{var}`-placeholder doc key. Both are derived from the node path so the
/// concrete key and the documented key stay in lockstep with
/// `ContractBody::TOPIC` (D1).
fn builder_leaf_key_parts(tree_id: &str, path: &[NodeSeg], leaf: &TopicLeaf) -> (String, String) {
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
    fn topic_and_family_are_version_qualified() {
        let expanded = compact_tokens(
            expand(quote! {
                version v0_1 {
                    drive {
                        struct Target { linear_x_mps: f32 }
                        topic target: command Target;
                    }
                }
                latest v0_1;
            })
            .expect("tree expands"),
        );
        assert!(
            expanded.contains("const TOPIC : & 'static str = \"v0.1/drive/target\""),
            "TOPIC must fold the version into the wire key (D1): {expanded}"
        );
        assert!(
            expanded.contains("const NAME : & 'static str = \"v0.1::drive::Target\""),
            "NAME must be the version-qualified type path (D1): {expanded}"
        );
        assert!(
            expanded.contains("const VERSION : & 'static str = \"v0.1\""),
            "VERSION must be the bare version, split from CONTRACT (\
             design §2): {expanded}"
        );
        assert!(
            expanded.contains("const CONTRACT : & 'static str = \"drive::Target\""),
            "CONTRACT must be the type path within its version, with no version \
             prefix: {expanded}"
        );
    }

    #[test]
    fn dynamic_node_topic_is_version_and_var_qualified() {
        let expanded = compact_tokens(
            expand(quote! {
                version v0_1 {
                    component(instance) {
                        motor(capability) {
                            enum Command { Stop }
                            topic command: command Command;
                        }
                    }
                }
                latest v0_1;
            })
            .expect("tree expands"),
        );
        assert!(
            expanded.contains(
                "const TOPIC : & 'static str = \"v0.1/component/{instance}/motor/{capability}/command\""
            ),
            "dynamic-node TOPIC must carry both the version and the {{var}} placeholders: {expanded}"
        );
        assert!(
            expanded.contains("const NAME : & 'static str = \"v0.1::component::motor::Command\""),
            "dynamic-node NAME must be the clean type path with no {{var}} placeholders - \
             dynamic-node vars are topic params, never type-path segments: {expanded}"
        );
        assert!(
            expanded.contains("const CONTRACT : & 'static str = \"component::motor::Command\""),
            "dynamic-node CONTRACT must also exclude every {{var}} placeholder: {expanded}"
        );
    }

    #[test]
    fn topic_builder_keys_are_version_qualified() {
        let expanded = compact_tokens(
            expand(quote! {
                version v0_1 {
                    drive {
                        struct Target { linear_x_mps: f32 }
                        topic target: command Target;
                    }
                }
                latest v0_1;
            })
            .expect("tree expands"),
        );
        assert!(
            expanded.contains("Topic :: new_static (\"v0.1/drive/target\")"),
            "the api-local topic builder must build the same version-qualified key as \
             ContractBody::TOPIC: {expanded}"
        );
    }

    #[test]
    fn protocol_keys_are_relative_to_the_protocol_name() {
        let expanded = compact_tokens(
            expand(quote! {
                protocol supervisor {
                    connect {
                        struct Hello { token: String }
                        topic hello: command Hello;
                    }
                }
            })
            .expect("protocol tree expands"),
        );
        assert!(
            expanded.contains("const TOPIC : & 'static str = \"supervisor/connect/hello\""),
            "a protocol key carries no version segment, only the protocol name: {expanded}"
        );
        assert!(
            expanded.contains("Topic :: new_static (\"supervisor/connect/hello\")"),
            "the builder must produce the same relative key as TOPIC: {expanded}"
        );
        assert!(
            expanded.contains("const ID : & 'static str = \"supervisor\""),
            "the protocol marker's ID is the protocol name: {expanded}"
        );
        assert!(
            expanded.contains("const NAME : & 'static str = \"supervisor::connect::Hello\""),
            "the protocol-qualified type identity mirrors API mode: {expanded}"
        );
        assert!(
            !expanded.contains("as latest"),
            "a protocol tree has no `latest` selection: {expanded}"
        );
    }

    #[test]
    fn protocol_mode_rejects_the_version_delta_forms() {
        for source in [
            "protocol supervisor { connect { replace struct Hello { token: String } } }",
            "protocol supervisor { connect { struct Hello { token: String } remove Hello; } }",
            "protocol supervisor { remove connect; }",
        ] {
            let tokens: TokenStream = source.parse().expect("test source tokenizes");
            let error = expand(tokens).expect_err("a delta form must be rejected");
            assert!(
                error.to_string().contains("no revision history"),
                "unexpected error for {source}: {error}"
            );
        }
    }

    #[test]
    fn protocol_and_version_modes_do_not_mix_in_one_invocation() {
        let error = expand(quote! {
            protocol supervisor { connect { struct Hello { token: String } } }
            version v0_1 { drive { struct Target { value: u8 } topic target: command Target; } }
            latest v0_1;
        })
        .expect_err("mixing the two modes must be rejected");
        assert!(
            error.to_string().contains("no `version` revisions"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn duplicate_protocol_names_in_one_invocation_are_rejected() {
        let error = expand(quote! {
            protocol supervisor { connect { struct Hello { token: String } } }
            protocol supervisor { connect { struct Hello { token: String } } }
        })
        .expect_err("a duplicate protocol name must be rejected");
        assert!(
            error.to_string().contains("duplicate protocol tree"),
            "unexpected error: {error}"
        );
    }

    #[test]
    fn a_protocol_name_must_be_a_lowercase_identifier() {
        let error = expand(quote! {
            protocol Supervisor { connect { struct Hello { token: String } } }
        })
        .expect_err("an uppercase protocol name must be rejected");
        assert!(
            error.to_string().contains("lowercase Rust identifier"),
            "unexpected error: {error}"
        );
    }

    /// API mode is untouched by protocol mode: its keys stay
    /// version-qualified. The rest of the API-mode suite in this module is the
    /// full regression; this one names the property explicitly.
    #[test]
    fn api_mode_keys_stay_version_qualified_alongside_protocol_mode() {
        let expanded = compact_tokens(
            expand(quote! {
                version v0_1 {
                    supervisor {
                        struct Hello { token: String }
                        topic hello: command Hello;
                    }
                }
                latest v0_1;
            })
            .expect("tree expands"),
        );
        assert!(
            expanded.contains("const TOPIC : & 'static str = \"v0.1/supervisor/hello\""),
            "an API-mode key keeps its version segment: {expanded}"
        );
        assert!(
            expanded.contains("const VERSION : & 'static str = \"v0.1\""),
            "an API-mode body keeps its dotted revision: {expanded}"
        );
    }

    #[test]
    fn duplicate_version_names_in_one_invocation_are_rejected() {
        let err = expand(quote! {
            version v0_1 { sample { struct Body { value: u8 } topic body: state Body; } }
            version v0_1 { sample { struct Body { value: u8 } topic body: state Body; } }
            latest v0_1;
        })
        .expect_err("a duplicate version name must be rejected");
        assert!(
            err.to_string().contains("duplicate API revision"),
            "unexpected error: {err}"
        );
    }

    #[test]
    fn inherited_revision_materializes_add_replace_and_remove() {
        let expanded = compact_tokens(
            expand(quote! {
                version v0_1 {
                    drive {
                        struct Target { value: u8 }
                        struct Removed { value: u8 }
                        topic target: command Target;
                        topic removed: state Removed;
                    }
                }
                version v0_2 extends v0_1 {
                    drive {
                        replace struct Target { value: u16 }
                        remove Removed;
                        remove removed;
                        struct Added { value: u32 }
                        topic added: state Added;
                    }
                }
                latest v0_2;
            })
            .expect("inherited tree expands"),
        );
        assert!(expanded.contains("pub mod v0_1"));
        assert!(expanded.contains("pub mod v0_2"));
        assert!(expanded.contains("\"v0.2/drive/target\""));
        assert!(expanded.contains("\"v0.2/drive/added\""));
        assert!(!expanded.contains("\"v0.2/drive/removed\""));
        assert!(expanded.contains("pub use v0_2 as latest"));
    }

    #[test]
    fn inherited_revision_reroots_absolute_parent_type_paths() {
        let expanded = compact_tokens(
            expand(quote! {
                version v0_1 {
                    tool {
                        struct Cursor { sequence: u64 }
                        struct Page { cursor: crate::v0_1::tool::Cursor }
                        topic page: state Page;
                    }
                }
                version v0_2 extends v0_1 {
                    tool { replace struct Cursor { sequence: u128 } }
                }
                latest v0_2;
            })
            .expect("inherited absolute paths are re-rooted"),
        );
        assert!(
            expanded.contains("cursor : crate :: v0_2 :: tool :: Cursor"),
            "the materialized child body must use the child revision's replacement: {expanded}"
        );
    }

    #[test]
    fn inherited_revision_rejects_silent_shadowing() {
        let error = expand(quote! {
            version v0_1 {
                sample { struct Body { value: u8 } topic body: state Body; }
            }
            version v0_2 extends v0_1 {
                sample { struct Body { value: u16 } }
            }
            latest v0_2;
        })
        .expect_err("same-path declarations require replace");
        assert!(
            error
                .to_string()
                .contains("prefix the declaration with `replace`")
        );
    }

    #[test]
    fn nonstandard_version_names_are_rejected() {
        for name in ["release_1", "preview2", "v0", "v01", "v1_beta"] {
            let source = format!(
                "version {name} {{ sample {{ struct Body {{ value: u8 }} topic value: state Body; }} }} latest {name};"
            );
            let tokens: TokenStream = source.parse().expect("test source tokenizes");
            let error = expand(tokens).expect_err("nonstandard API version must fail");
            assert!(
                error.to_string().contains("API revision"),
                "unexpected error for {name}: {error}"
            );
        }
    }

    #[test]
    fn expansion_emits_root_contract_manifest() {
        let expanded = compact_tokens(
            expand(quote! {
                version v0_1 {
                    sample {
                        struct Body { value: u8 }
                        topic body: state Body;
                    }
                }
                version v0_2 extends v0_1 {
                    sample {
                        struct Added { value: u16 }
                        topic added: state Added;
                    }
                }
                latest v0_2;
            })
            .expect("tree expands"),
        );

        assert!(
            expanded.contains("pub const API_CONTRACT_MANIFEST"),
            "root manifest const should be emitted: {expanded}"
        );
        assert!(
            expanded.contains("# [cfg (test)] # [doc (hidden)] pub const API_CONTRACT_MANIFEST"),
            "the manifest const (and its two supporting types) must be \
             #[cfg(test)]-gated: {expanded}"
        );
        assert!(
            expanded.contains(
                "# [cfg (test)] # [doc (hidden)] # [derive (Clone , Copy , Debug , Eq , \
                 PartialEq)] pub struct ApiContractManifestVersion"
            ),
            "ApiContractManifestVersion must be #[cfg(test)]-gated alongside the const: {expanded}"
        );
        assert!(
            expanded.contains(
                "# [cfg (test)] # [doc (hidden)] # [derive (Clone , Copy , Debug , Eq , \
                 PartialEq)] pub struct ApiContractManifestContract"
            ),
            "ApiContractManifestContract must be #[cfg(test)]-gated alongside the const: {expanded}"
        );
        assert!(
            expanded.contains("name : \"v0.2\""),
            "child revision should be represented in the manifest: {expanded}"
        );
        assert!(
            expanded.contains("family : \"v0.1::sample::Body\""),
            "manifest family is the version-qualified contract identity (D1): {expanded}"
        );
        assert!(
            expanded.contains("topic : \"v0.2/sample/body\""),
            "manifest topic is the version-qualified wire key (D1): {expanded}"
        );
        assert!(
            expanded.contains("family : \"v0.2::sample::Body\""),
            "each version's contracts get their own version-qualified name: {expanded}"
        );
        assert!(
            expanded.contains("topic : \"v0.2/sample/body\""),
            "each version's contracts get their own version-qualified key: {expanded}"
        );
        assert!(
            expanded.contains("pub use v0_2 as latest"),
            "the selected concrete revision should be exported as latest: {expanded}"
        );
    }

    #[test]
    fn query_request_and_response_share_one_version_qualified_topic() {
        let expanded = compact_tokens(
            expand(quote! {
                version v0_1 {
                    asset {
                        struct GetRequest { path: String }
                        enum GetResponse { Missing }
                        topic get: query GetRequest => GetResponse;
                    }
                }
                latest v0_1;
            })
            .expect("tree expands"),
        );
        assert_eq!(
            expanded
                .matches("const TOPIC : & 'static str = \"v0.1/asset/get\"")
                .count(),
            2,
            "both the request and response bodies of a query topic share its \
             version-qualified key: {expanded}"
        );
        assert!(
            expanded.contains("const NAME : & 'static str = \"v0.1::asset::GetRequest\""),
            "the request body gets its own type-path NAME: {expanded}"
        );
        assert!(
            expanded.contains("const NAME : & 'static str = \"v0.1::asset::GetResponse\""),
            "the response body gets its own type-path NAME, distinct from the \
             request's even though they share one TOPIC: {expanded}"
        );
        assert!(
            expanded.contains("const CONTRACT : & 'static str = \"asset::GetRequest\""),
            "the request body's CONTRACT is its own type path, version excluded: {expanded}"
        );
        assert!(
            expanded.contains("const CONTRACT : & 'static str = \"asset::GetResponse\""),
            "the response body's CONTRACT is distinct from the request's: {expanded}"
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
                version v0_1 {
                    a(x) {
                        b(y) {
                            c(z) {
                                struct Body { v: u8 }
                                topic leaf: state Body;
                            }
                        }
                    }
                }
                latest v0_1;
            })
            .expect("tree expands"),
        );

        assert!(
            !expanded.contains("super :: super"),
            "no reference should ever chain more than one `super::` hop, on \
             either the type-tree or the builder-tree side: {expanded}"
        );
        assert!(
            expanded.contains("const TOPIC : & 'static str = \"v0.1/a/{x}/b/{y}/c/{z}/leaf\""),
            "the three-level dynamic path must still be fully version- and \
             var-qualified: {expanded}"
        );
        assert!(
            expanded.contains("const NAME : & 'static str = \"v0.1::a::b::c::Body\""),
            "NAME excludes every dynamic var from the type path, unlike TOPIC: {expanded}"
        );
        assert!(
            expanded.contains("const CONTRACT : & 'static str = \"a::b::c::Body\""),
            "CONTRACT excludes both the version and every dynamic var: {expanded}"
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
