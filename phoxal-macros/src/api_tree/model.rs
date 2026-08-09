//! The tree model: the nodes and endpoint descriptors one semantic macro
//! invocation declares, plus the delta algebra that
//! materializes an `extends` child into a complete concrete tree.
//!
//! Nothing here emits a module or a builder - the model is what the grammar
//! produces and what every codegen pass reads.

use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::Ident;

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
    /// Whether this declaration selects the train's facade.  Keeping the
    /// selection on the revision makes the source declaration self-contained;
    /// the old trailing `latest vN_M;` form is still accepted by the
    /// compatibility parser, but strict trees use `latest version vM.m ...`.
    pub(super) latest: bool,
    /// Whether strict mode selected this revision with the prefix spelling
    /// `latest version vM.m ...`.
    pub(super) latest_prefix: bool,
    pub(super) parent: Option<Ident>,
    pub(super) nodes: Vec<Node>,
    pub(super) removals: Vec<Removal>,
}

/// One node in the api tree: a `name { … }` (static) or `name(var) { … }`
/// (dynamic) block that may hold endpoint declarations and nested child nodes.
#[derive(Clone)]
pub(super) struct Node {
    pub(super) name: Ident,
    pub(super) replace: bool,
    /// The dynamic variable bound by this node (`None` for a static node). When
    /// present, the node contributes `name/{var}` to keys and a var-taking builder
    /// method.
    pub(super) var: Option<Ident>,
    pub(super) topics: Vec<TopicDef>,
    pub(super) children: Vec<Node>,
    pub(super) removals: Vec<Removal>,
}

#[derive(Clone)]
pub(super) struct TopicDef {
    pub(super) replace: bool,
    pub(super) leaf: TopicLeaf,
    pub(super) kind: TopicKind,
    /// The endpoint semantic determines temporal capability and transport
    /// family. Direction is separately explicit in the source grammar.
    pub(super) semantic: SemanticKind,
    /// Whether the endpoint owner publishes its payload.
    pub(super) owner_publishes: bool,
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

/// One fixed semantic endpoint category from the public macro grammar.
#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum SemanticKind {
    Setpoint,
    Stream,
    State,
    Event,
    Sample,
    Query,
}

impl TopicDef {
    pub(super) fn endpoint_ident(&self) -> Ident {
        let leaf = self.leaf.method_ident();
        let name = leaf
            .to_string()
            .split('_')
            .filter(|part| !part.is_empty())
            .map(|part| {
                let mut part = part.to_string();
                if let Some(first) = part.get_mut(0..1) {
                    first.make_ascii_uppercase();
                }
                part
            })
            .collect::<String>();
        Ident::new(&format!("{name}Endpoint"), leaf.span())
    }

    pub(super) fn endpoint_kind(&self) -> TokenStream {
        match self.semantic {
            SemanticKind::State => quote! { ::phoxal_bus::EndpointKind::State },
            SemanticKind::Sample => quote! { ::phoxal_bus::EndpointKind::Sample },
            SemanticKind::Event => quote! { ::phoxal_bus::EndpointKind::Event },
            SemanticKind::Stream => quote! { ::phoxal_bus::EndpointKind::Stream },
            SemanticKind::Setpoint => quote! { ::phoxal_bus::EndpointKind::Setpoint },
            SemanticKind::Query => quote! { ::phoxal_bus::EndpointKind::Query },
        }
    }

    /// The stable manifest spelling for the endpoint's fixed semantic kind.
    ///
    /// Keep this beside [`endpoint_kind`](Self::endpoint_kind) so the emitted
    /// enum value and the fingerprint input cannot drift apart.
    pub(super) fn endpoint_kind_name(&self) -> &'static str {
        match self.semantic {
            SemanticKind::State => "State",
            SemanticKind::Sample => "Sample",
            SemanticKind::Event => "Event",
            SemanticKind::Stream => "Stream",
            SemanticKind::Setpoint => "Setpoint",
            SemanticKind::Query => "Query",
        }
    }

    /// The transport family emitted on the body and consumed by the semantic
    /// bus scheduler. Keeping this calculation in the source model means the
    /// generated manifest cannot accidentally classify an overridden topic by
    /// its temporal role.
    pub(super) fn delivery_family(&self) -> TokenStream {
        self.semantic.delivery_family()
    }

    /// The stable manifest spelling for the transport family.
    pub(super) fn delivery_family_name(&self) -> &'static str {
        self.semantic.delivery_family_name()
    }
}

impl SemanticKind {
    /// The transport family selected by this endpoint semantic.
    pub(super) fn delivery_family(self) -> TokenStream {
        match self {
            Self::State => quote! { ::phoxal_bus::DeliveryFamily::State },
            Self::Sample => quote! { ::phoxal_bus::DeliveryFamily::Sample },
            Self::Setpoint => quote! { ::phoxal_bus::DeliveryFamily::Setpoint },
            Self::Stream | Self::Event => quote! { ::phoxal_bus::DeliveryFamily::Stream },
            Self::Query => quote! { ::phoxal_bus::DeliveryFamily::Query },
        }
    }

    /// The transport semantic family independent of temporal stamping.
    pub(super) fn delivery_family_name(self) -> &'static str {
        match self {
            Self::State => "State",
            Self::Sample => "Sample",
            Self::Setpoint => "Setpoint",
            Self::Stream | Self::Event => "Stream",
            Self::Query => "Query",
        }
    }

    /// The transport marker derived from the endpoint semantic. Events share
    /// ordered transport with streams while retaining their step-time marker.
    pub(super) fn delivery_marker_trait(self) -> TokenStream {
        match self {
            Self::Setpoint => quote! { ::phoxal_bus::SetpointDeliveryContract },
            Self::Stream | Self::Event => {
                quote! { ::phoxal_bus::StreamDeliveryContract }
            }
            Self::Sample => quote! { ::phoxal_bus::SampleDeliveryContract },
            Self::State => {
                quote! { ::phoxal_bus::StateDeliveryContract }
            }
            Self::Query => {
                unreachable!("query bodies have no pub/sub delivery marker")
            }
        }
    }

    pub(super) fn semantic_marker_trait(self) -> Option<TokenStream> {
        match self {
            Self::Setpoint => Some(quote! { ::phoxal_bus::SetpointContract }),
            Self::Stream => Some(quote! { ::phoxal_bus::StreamContract }),
            Self::State => Some(quote! { ::phoxal_bus::StateContract }),
            Self::Event => Some(quote! { ::phoxal_bus::EventContract }),
            Self::Sample => Some(quote! { ::phoxal_bus::SampleContract }),
            Self::Query => None,
        }
    }
}

#[derive(Clone)]
pub(super) enum TopicKind {
    PubSub(BodyPath),
    Query {
        request: BodyPath,
        response: BodyPath,
    },
}

/// A payload path supplied by the normal domain/version modules. The old tree
/// grammar produces a one-segment path; the semantic API grammar accepts a
/// complete Rust path such as `crate::versions::v0_2::drive::Target`.
#[derive(Clone)]
pub(super) struct BodyPath {
    pub(super) path: syn::Path,
}

impl BodyPath {
    /// A deterministic source identity for manifest/fingerprint evidence.
    ///
    /// The path is intentionally retained as authored rather than reduced to
    /// its leaf: two endpoints may reuse one payload, and two payloads may
    /// share a leaf name in different modules.
    pub(super) fn identity(&self) -> String {
        self.path.to_token_stream().to_string().replace(' ', "")
    }
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

impl Version {
    /// Materialize this revision against its parent's already-concrete tree:
    /// inherit everything, apply the `remove` deltas, then merge additions and
    /// `replace`s.
    ///
    /// `parent` is passed alongside `base` because only the caller that looked
    /// `base` up knows which declared revision it came from.
    pub(super) fn materialize_from(
        &self,
        base: &[Node],
        _parent: &Ident,
    ) -> syn::Result<Vec<Node>> {
        let mut nodes = base.to_vec();
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

    /// Merge one delta node into its inherited counterpart. A same-path topic
    /// must be explicitly `replace`d: silently shadowing an inherited
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
    /// name exactly one of this node's topics or children - an
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
        let topic_index = self
            .topics
            .iter()
            .position(|item| item.leaf.method_ident() == *head);
        let child_index = self.children.iter().position(|item| item.name == *head);
        let matches = usize::from(topic_index.is_some()) + usize::from(child_index.is_some());
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
        if let Some(index) = topic_index {
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
