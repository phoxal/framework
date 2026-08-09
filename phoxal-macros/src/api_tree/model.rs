//! The tree model: the nodes, wire bodies, topics, and roles one
//! `phoxal_api_tree!` invocation declares, plus the delta algebra that
//! materializes an `extends` child into a complete concrete tree.
//!
//! Nothing here emits a module or a builder - the model is what the grammar
//! produces and what every codegen pass reads.

use proc_macro2::TokenStream;
use quote::quote;
use syn::visit_mut::{self, VisitMut};
use syn::{Ident, ItemEnum, ItemStruct};

use super::grammar::VERSION_HAS_NO_PARENT;

/// One `protocol <name> { … }` tree.
///
/// A protocol has no revision history and no version segment. Its `name` is
/// both the generated module and the tree's identity, and it is the leading
/// wire-key segment - exactly the slot the dotted revision occupies in API
/// mode.
pub(super) struct Protocol {
    pub(super) name: Ident,
    pub(super) nodes: Vec<Node>,
}

/// One `version vM_N [extends vX_Y] { … }` revision, as authored. A revision
/// with a parent carries only the delta; [`Version::materialize_from`] turns it
/// into the complete concrete tree that is actually emitted.
pub(super) struct Version {
    pub(super) name: Ident,
    /// The dotted wire spelling of `name` (`v0_1` -> `"v0.1"`), which is both
    /// this revision's identity and its leading wire-key segment.
    pub(super) wire_id: String,
    pub(super) parent: Option<Ident>,
    pub(super) nodes: Vec<Node>,
    pub(super) removals: Vec<Removal>,
}

/// One node in the api tree: a `name { … }` (static) or `name(var) { … }`
/// (dynamic) block that may hold types, topics, and nested child nodes.
#[derive(Clone)]
pub(super) struct Node {
    pub(super) name: Ident,
    pub(super) replace: bool,
    /// The dynamic variable bound by this node (`None` for a static node). When
    /// present, the node contributes `name/{var}` to keys and a var-taking builder
    /// method.
    pub(super) var: Option<Ident>,
    pub(super) types: Vec<TypeDef>,
    pub(super) topics: Vec<TopicDef>,
    pub(super) children: Vec<Node>,
    pub(super) removals: Vec<Removal>,
}

#[derive(Clone)]
pub(super) struct TypeDef {
    pub(super) replace: bool,
    pub(super) item: TypeItem,
}

#[derive(Clone)]
pub(super) enum TypeItem {
    Struct(ItemStruct),
    Enum(ItemEnum),
}

#[derive(Clone)]
pub(super) struct TopicDef {
    pub(super) replace: bool,
    pub(super) leaf: TopicLeaf,
    pub(super) kind: TopicKind,
    /// The semantic and temporal role declared by the topic's role keyword.
    /// `command`, `stream`, `state`, `event`, `measurement`, and `diagnostic` all produce a
    /// [`TopicKind::PubSub`] on the wire, while `query` produces a
    /// [`TopicKind::Query`]. The role selects the SIDE BRAND in the generated
    /// builders: per (role, side) a leaf returns `Publish` / `Subscribe` /
    /// `AskQuery` / `ServeQuery`, so the public (client) and owner builders
    /// return different branded topics. It is also emitted as
    /// `ContractBody::ROLE` plus the matching temporal-role marker impl, which
    /// is what fixes the robot time a publisher of the body can express. The
    /// optional `delivery` clause changes only the transport family, allowing
    /// temporal roles such as `event` to use ordered stream delivery without
    /// inventing a new temporal role.
    pub(super) role: TopicRole,
    /// Optional transport override. Temporal role and side branding continue
    /// to come from `role`; this field only changes delivery admission/storage.
    pub(super) delivery: Option<DeliveryOverride>,
}

/// One `remove <path>;` declaration. The head segment is split out from the
/// rest so both the diagnostic span and the descent are total - there is no
/// path length at which either has to index.
#[derive(Clone)]
pub(super) struct Removal {
    pub(super) head: Ident,
    pub(super) rest: Vec<Ident>,
}

#[derive(Clone)]
pub(super) enum TopicLeaf {
    Named(Ident),
    /// `topic self: …` - the body binds to the node path itself instead of
    /// appending a leaf segment, for framework infrastructure topics such as
    /// `logs/{participant_id}`.
    Node,
}

impl TopicLeaf {
    /// The builder method (and delta-matching) name for this leaf. A `self`
    /// leaf has no segment of its own, so it is addressed as `topic`.
    pub(super) fn method_ident(&self) -> Ident {
        match self {
            TopicLeaf::Named(ident) => ident.clone(),
            TopicLeaf::Node => quote::format_ident!("topic"),
        }
    }

    /// This leaf's wire key below the tree identity: the owning node's key
    /// prefix plus the leaf segment, or the node path alone for a `self` leaf.
    pub(super) fn key(&self, node_key_prefix: &str) -> String {
        match self {
            TopicLeaf::Named(leaf) => format!("{node_key_prefix}/{leaf}"),
            TopicLeaf::Node => node_key_prefix.to_string(),
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
pub(super) enum TopicRole {
    Command,
    Stream,
    State,
    Event,
    Measurement,
    Diagnostic,
    Query,
    WorldClock,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum DeliveryOverride {
    State,
    Sample,
    Setpoint,
    Stream,
}

impl DeliveryOverride {
    pub(super) fn bus_variant(self) -> TokenStream {
        match self {
            Self::State => quote! { ::phoxal_bus::DeliveryFamily::State },
            Self::Sample => quote! { ::phoxal_bus::DeliveryFamily::Sample },
            Self::Setpoint => quote! { ::phoxal_bus::DeliveryFamily::Setpoint },
            Self::Stream => quote! { ::phoxal_bus::DeliveryFamily::Stream },
        }
    }

    pub(super) fn marker_trait(self) -> TokenStream {
        match self {
            Self::State => quote! { ::phoxal_bus::StateDeliveryContract },
            Self::Sample => quote! { ::phoxal_bus::SampleDeliveryContract },
            Self::Setpoint => quote! { ::phoxal_bus::SetpointDeliveryContract },
            Self::Stream => quote! { ::phoxal_bus::StreamDeliveryContract },
        }
    }
}

impl TopicRole {
    /// The `phoxal_bus::TopicRole` variant path this role maps to.
    pub(super) fn bus_variant(self) -> TokenStream {
        match self {
            TopicRole::Command => quote! { ::phoxal_bus::TopicRole::Command },
            TopicRole::Stream => quote! { ::phoxal_bus::TopicRole::Stream },
            TopicRole::State | TopicRole::Event | TopicRole::WorldClock => {
                quote! { ::phoxal_bus::TopicRole::State }
            }
            TopicRole::Measurement => quote! { ::phoxal_bus::TopicRole::Measurement },
            TopicRole::Diagnostic => quote! { ::phoxal_bus::TopicRole::Diagnostic },
            TopicRole::Query => quote! { ::phoxal_bus::TopicRole::Query },
        }
    }

    /// The transport semantic family independent of temporal stamping.
    pub(super) fn bus_delivery(self) -> TokenStream {
        match self {
            TopicRole::Command => quote! { ::phoxal_bus::DeliveryFamily::Setpoint },
            TopicRole::Stream => quote! { ::phoxal_bus::DeliveryFamily::Stream },
            TopicRole::State | TopicRole::WorldClock | TopicRole::Diagnostic => {
                quote! { ::phoxal_bus::DeliveryFamily::State }
            }
            TopicRole::Event => quote! { ::phoxal_bus::DeliveryFamily::Stream },
            TopicRole::Measurement => quote! { ::phoxal_bus::DeliveryFamily::Sample },
            TopicRole::Query => quote! { ::phoxal_bus::DeliveryFamily::Query },
        }
    }

    /// The transport marker emitted independently from the temporal role
    /// marker. This lets an event, diagnostic, or world-clock body opt into
    /// ordered delivery without inventing another temporal role.
    pub(super) fn delivery_marker_trait(self) -> TokenStream {
        match self {
            TopicRole::Command => quote! { ::phoxal_bus::SetpointDeliveryContract },
            TopicRole::Stream | TopicRole::Event => {
                quote! { ::phoxal_bus::StreamDeliveryContract }
            }
            TopicRole::Measurement => quote! { ::phoxal_bus::SampleDeliveryContract },
            TopicRole::State | TopicRole::WorldClock | TopicRole::Diagnostic => {
                quote! { ::phoxal_bus::StateDeliveryContract }
            }
            TopicRole::Query => {
                unreachable!("query bodies have no pub/sub delivery marker")
            }
        }
    }

    /// The temporal-role marker trait a body of this role implements. `query`
    /// has none: a request/response leg expresses no robot time and is served
    /// through the runner, not a publisher handle.
    pub(super) fn marker_trait(self) -> Option<TokenStream> {
        match self {
            TopicRole::Command => Some(quote! { ::phoxal_bus::CommandContract }),
            TopicRole::Stream => Some(quote! { ::phoxal_bus::StreamContract }),
            TopicRole::State | TopicRole::Event => Some(quote! { ::phoxal_bus::StateContract }),
            TopicRole::Measurement => Some(quote! { ::phoxal_bus::MeasurementContract }),
            TopicRole::Diagnostic => Some(quote! { ::phoxal_bus::DiagnosticContract }),
            TopicRole::WorldClock => Some(quote! { ::phoxal_bus::WorldClockContract }),
            TopicRole::Query => None,
        }
    }

    /// Whether the owning participant publishes this role (as opposed to
    /// subscribing it).
    pub(super) fn owner_publishes(self) -> bool {
        !matches!(self, TopicRole::Command | TopicRole::Stream)
    }
}

#[derive(Clone)]
pub(super) enum TopicKind {
    PubSub(Ident),
    Query { request: Ident, response: Ident },
}

/// One fully resolved tree ready to emit, from either mode.
///
/// `id` is the tree's identity AND its leading wire-key segment: the dotted
/// revision (`"v0.1"`) for an API revision, the protocol name (`"supervisor"`)
/// for a protocol. Everything below this point is mode-agnostic - the two modes
/// differ only in how a tree is parsed, validated, and identified, never in how
/// its modules, bodies, or builders are shaped.
pub(super) struct MaterializedTree {
    pub(super) module: Ident,
    pub(super) id: String,
    pub(super) doc: String,
    pub(super) nodes: Vec<Node>,
}

impl TypeDef {
    pub(super) fn ident(&self) -> &Ident {
        match &self.item {
            TypeItem::Struct(item) => &item.ident,
            TypeItem::Enum(item) => &item.ident,
        }
    }
}

impl Version {
    /// Materialize this revision against its parent's already-concrete tree:
    /// inherit everything, re-root inherited absolute paths onto this revision,
    /// apply the `remove` deltas, then merge the additions and `replace`s.
    ///
    /// `parent` is passed alongside `base` because only the caller that looked
    /// `base` up knows which declared revision it came from.
    pub(super) fn materialize_from(&self, base: &[Node], parent: &Ident) -> syn::Result<Vec<Node>> {
        let mut nodes = base.to_vec();
        Node::reroot_revision_paths(&mut nodes, parent, &self.name);
        for removal in &self.removals {
            Node::remove_path_in(&mut nodes, &removal.head, &removal.rest)?;
        }
        Node::merge_all(&mut nodes, &self.nodes)?;
        Ok(nodes)
    }
}

impl Node {
    /// Append this node's name to a `::`-joined family path. Dynamic vars never
    /// appear: they are topic params, not type-path segments.
    pub(super) fn family_path(&self, prefix: &str) -> String {
        join_seg(prefix, "::", &self.name.to_string())
    }

    /// Append this node's wire-key contribution to a `/`-joined key prefix:
    /// `name` for a static node, `name/{var}` for a dynamic one.
    pub(super) fn key_prefix(&self, prefix: &str) -> String {
        let name = self.name.to_string();
        let seg = match &self.var {
            Some(var) => format!("{name}/{{{var}}}"),
            None => name,
        };
        join_seg(prefix, "/", &seg)
    }

    /// Reject the delta forms (`replace` / `remove`) anywhere in a tree that has
    /// nothing to delta against. `reason` is the tail of the diagnostic, naming why
    /// this particular tree has no parent revision.
    pub(super) fn reject_delta_forms(
        nodes: &[Node],
        removals: &[Removal],
        reason: &'static str,
    ) -> syn::Result<()> {
        if let Some(removal) = removals.first() {
            return Err(syn::Error::new_spanned(
                &removal.head,
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
                    &removal.head,
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
            Node::reject_delta_forms(&node.children, &[], reason)?;
        }
        Ok(())
    }

    /// Re-root absolute paths authored against the parent revision when its types
    /// are materialized into a child. Without this pass, an inherited body such as
    /// `struct Page { cursor: crate::v0_1::tool::Cursor }` would keep referring to
    /// the parent's `Cursor` even after the child explicitly replaced that type.
    fn reroot_revision_paths(nodes: &mut [Node], parent: &Ident, child: &Ident) {
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

    /// Merge a delta node list into an inherited one. A same-named node is
    /// merged recursively unless it is `replace`d wholesale; a new node is
    /// appended and must itself be delta-free.
    fn merge_all(base: &mut Vec<Node>, deltas: &[Node]) -> syn::Result<()> {
        for delta in deltas {
            let existing = base.iter().position(|node| node.name == delta.name);
            match (existing, delta.replace) {
                (Some(index), true) => {
                    let mut replacement = delta.clone();
                    replacement.replace = false;
                    Node::reject_delta_forms(
                        std::slice::from_ref(&replacement),
                        &[],
                        VERSION_HAS_NO_PARENT,
                    )?;
                    base[index] = replacement;
                }
                (Some(index), false) => base[index].merge(delta)?,
                (None, true) => {
                    return Err(syn::Error::new_spanned(
                        &delta.name,
                        "`replace` target does not exist in the parent revision",
                    ));
                }
                (None, false) => {
                    Node::reject_delta_forms(
                        std::slice::from_ref(delta),
                        &[],
                        VERSION_HAS_NO_PARENT,
                    )?;
                    base.push(delta.clone());
                }
            }
        }
        Ok(())
    }

    /// Merge one delta node into its inherited counterpart. A same-path type or
    /// topic must be explicitly `replace`d: silently shadowing an inherited
    /// declaration would change the contract without saying so.
    fn merge(&mut self, delta: &Node) -> syn::Result<()> {
        if self.var.as_ref().map(Ident::to_string) != delta.var.as_ref().map(Ident::to_string) {
            return Err(syn::Error::new_spanned(
                &delta.name,
                "an inherited node must keep the same static/dynamic binding",
            ));
        }
        for removal in &delta.removals {
            self.remove_path(&removal.head, &removal.rest)?;
        }
        for delta_type in &delta.types {
            let ident = delta_type.ident();
            let existing = self.types.iter().position(|item| item.ident() == ident);
            match (existing, delta_type.replace) {
                (Some(index), true) => {
                    let mut replacement = delta_type.clone();
                    replacement.replace = false;
                    self.types[index] = replacement;
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
                (None, false) => self.types.push(delta_type.clone()),
            }
        }
        for delta_topic in &delta.topics {
            let ident = delta_topic.leaf.method_ident();
            let existing = self
                .topics
                .iter()
                .position(|item| item.leaf.method_ident() == ident);
            match (existing, delta_topic.replace) {
                (Some(index), true) => {
                    let mut replacement = delta_topic.clone();
                    replacement.replace = false;
                    self.topics[index] = replacement;
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
                (None, false) => self.topics.push(delta_topic.clone()),
            }
        }
        Node::merge_all(&mut self.children, &delta.children)
    }

    /// Apply one `remove <head>[::<rest>];` against a node list. A path that
    /// ends here removes the whole node; a longer one descends into it.
    fn remove_path_in(nodes: &mut Vec<Node>, head: &Ident, rest: &[Ident]) -> syn::Result<()> {
        let Some(index) = nodes.iter().position(|node| node.name == *head) else {
            return Err(syn::Error::new_spanned(
                head,
                "`remove` path does not exist in the parent revision",
            ));
        };
        let Some((next, rest)) = rest.split_first() else {
            nodes.remove(index);
            return Ok(());
        };
        nodes[index].remove_path(next, rest)
    }

    /// Apply one `remove` path relative to this node. The final segment must
    /// name exactly one of this node's types, topics, or children - an
    /// ambiguous name is rejected rather than guessed at.
    fn remove_path(&mut self, head: &Ident, rest: &[Ident]) -> syn::Result<()> {
        if let Some((next, rest)) = rest.split_first() {
            let Some(child) = self.children.iter_mut().find(|child| child.name == *head) else {
                return Err(syn::Error::new_spanned(
                    head,
                    "`remove` path does not exist in the parent revision",
                ));
            };
            return child.remove_path(next, rest);
        }
        let type_index = self.types.iter().position(|item| item.ident() == head);
        let topic_index = self
            .topics
            .iter()
            .position(|item| item.leaf.method_ident() == *head);
        let child_index = self.children.iter().position(|item| item.name == *head);
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
            self.types.remove(index);
        } else if let Some(index) = topic_index {
            self.topics.remove(index);
        } else if let Some(index) = child_index {
            self.children.remove(index);
        }
        Ok(())
    }
}

/// Join two path/key segments with `sep`; if `prefix` is empty, return `seg`
/// alone, so a root-level node contributes no leading separator.
fn join_seg(prefix: &str, sep: &str, seg: &str) -> String {
    if prefix.is_empty() {
        seg.to_string()
    } else {
        format!("{prefix}{sep}{seg}")
    }
}
