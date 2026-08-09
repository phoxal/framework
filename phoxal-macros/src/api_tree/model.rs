//! The generated tree model shared by materialized Robot API revisions and
//! process protocols.
//!
//! Nothing here emits a module or a builder - the model is what the grammar
//! produces and what every codegen pass reads.

use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::Ident;

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

/// One already-materialized Robot API revision. Fragment inheritance is
/// resolved into endpoint maps before this representation is built.
pub(super) struct ConcreteVersion {
    pub(super) name: Ident,
    /// The dotted wire spelling of `name` (`v0_1` -> `"v0.1"`), which is both
    /// this revision's identity and its leading wire-key segment.
    pub(super) wire_id: String,
    pub(super) major: u16,
    pub(super) minor: u16,
    pub(super) nodes: Vec<Node>,
}

/// One node in the api tree: a `name { … }` (static) or `name(var) { … }`
/// (dynamic) block that may hold endpoint declarations and nested child nodes.
#[derive(Clone)]
pub(super) struct Node {
    pub(super) name: Ident,
    /// The dynamic variable bound by this node (`None` for a static node). When
    /// present, the node contributes `name/{var}` to keys and a var-taking builder
    /// method.
    pub(super) var: Option<Ident>,
    pub(super) topics: Vec<TopicDef>,
    pub(super) children: Vec<Node>,
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

    /// The transport family emitted on the body and consumed by the semantic
    /// bus scheduler. Keeping this calculation in the source model means the
    /// generated manifest cannot accidentally classify an overridden topic by
    /// its temporal role.
    pub(super) fn delivery_family(&self) -> TokenStream {
        self.semantic.delivery_family()
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

/// A resolved payload path. Robot API fragments start with a local sibling
/// type name and the tree materializer qualifies it; protocols accept complete
/// Rust paths directly.
#[derive(Clone)]
pub(super) struct BodyPath {
    pub(super) path: syn::Path,
}

impl BodyPath {
    /// A deterministic source identity for endpoint manifest evidence.
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
    /// Authored module prefix for Robot API payloads. Protocol payload paths
    /// are already explicit and have no revision provenance to resolve.
    pub(super) source: Option<syn::Path>,
    pub(super) nodes: Vec<Node>,
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
