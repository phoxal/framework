//! The generated tree model for materialized contract families.
//!
//! Nothing here emits a module or a builder - the model is what the grammar
//! produces and what every codegen pass reads.

use proc_macro2::TokenStream;
use quote::{ToTokens, quote};
use syn::Ident;

/// One node in the protocol tree: a `name { … }` (static) or `name(var) { … }`
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
    pub(super) leaf: TopicLeaf,
    /// The descriptor is the complete endpoint contract. Each variant is a
    /// legal state, so direction, arity, and owner role cannot disagree.
    pub(super) descriptor: Descriptor,
}

#[derive(Clone)]
pub(super) enum TopicLeaf {
    Named(Ident),
    /// `self: …` - the body binds to the node path itself instead of
    /// appending a leaf segment, so a section whose whole path is the wire key
    /// (`runtime/logs`, `supervisor/snapshot`) says so directly.
    Node,
}

impl TopicLeaf {
    /// The builder method name for this leaf. A `self` leaf has no segment of
    /// its own, so it is addressed as `topic`.
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

/// One legal descriptor from the public protocol grammar.
#[derive(Clone)]
pub(super) enum Descriptor {
    State(BodyPath),
    Sample(BodyPath),
    Event(BodyPath),
    Setpoint(BodyPath),
    Stream {
        body: BodyPath,
        direction: StreamDirection,
    },
    Query {
        request: BodyPath,
        response: BodyPath,
    },
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamDirection {
    In,
    Out,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum OwnerRole {
    Publishes,
    Consumes,
    Serves,
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
        self.descriptor.endpoint_kind()
    }

    /// The transport family emitted on the body and consumed by the semantic
    /// bus scheduler. Keeping this calculation in the source model means the
    /// generated manifest cannot accidentally classify an overridden topic by
    /// its temporal role.
    pub(super) fn delivery_family(&self) -> TokenStream {
        self.descriptor.delivery_family()
    }
}

impl Descriptor {
    pub(super) fn endpoint_kind(&self) -> TokenStream {
        match self {
            Self::State(_) => quote! { ::phoxal_bus::EndpointKind::State },
            Self::Sample(_) => quote! { ::phoxal_bus::EndpointKind::Sample },
            Self::Event(_) => quote! { ::phoxal_bus::EndpointKind::Event },
            Self::Stream { .. } => quote! { ::phoxal_bus::EndpointKind::Stream },
            Self::Setpoint(_) => quote! { ::phoxal_bus::EndpointKind::Setpoint },
            Self::Query { .. } => quote! { ::phoxal_bus::EndpointKind::Query },
        }
    }

    /// The transport family selected by this descriptor.
    pub(super) fn delivery_family(&self) -> TokenStream {
        match self {
            Self::State(_) => quote! { ::phoxal_bus::DeliveryFamily::State },
            Self::Sample(_) => quote! { ::phoxal_bus::DeliveryFamily::Sample },
            Self::Setpoint(_) => quote! { ::phoxal_bus::DeliveryFamily::Setpoint },
            Self::Stream { .. } | Self::Event(_) => {
                quote! { ::phoxal_bus::DeliveryFamily::Stream }
            }
            Self::Query { .. } => quote! { ::phoxal_bus::DeliveryFamily::Query },
        }
    }

    /// The transport marker derived from the descriptor. Events share
    /// ordered transport with streams while retaining their step-time marker.
    pub(super) fn delivery_marker_trait(&self) -> TokenStream {
        match self {
            Self::Setpoint(_) => quote! { ::phoxal_bus::SetpointDeliveryContract },
            Self::Stream { .. } | Self::Event(_) => {
                quote! { ::phoxal_bus::StreamDeliveryContract }
            }
            Self::Sample(_) => quote! { ::phoxal_bus::SampleDeliveryContract },
            Self::State(_) => {
                quote! { ::phoxal_bus::StateDeliveryContract }
            }
            Self::Query { .. } => {
                unreachable!("query bodies have no pub/sub delivery marker")
            }
        }
    }

    pub(super) fn semantic_marker_trait(&self) -> Option<TokenStream> {
        match self {
            Self::Setpoint(_) => Some(quote! { ::phoxal_bus::SetpointContract }),
            Self::Stream { .. } => Some(quote! { ::phoxal_bus::StreamContract }),
            Self::State(_) => Some(quote! { ::phoxal_bus::StateContract }),
            Self::Event(_) => Some(quote! { ::phoxal_bus::EventContract }),
            Self::Sample(_) => Some(quote! { ::phoxal_bus::SampleContract }),
            Self::Query { .. } => None,
        }
    }

    /// The endpoint role an external client may take.
    ///
    /// This is derived from the same descriptor state as the client/owner topic
    /// brands. Keeping it on the generated descriptor lets generic clients
    /// distinguish the otherwise-identical `Stream<_, In>` and
    /// `Stream<_, Out>` semantic markers without copying an endpoint allowlist.
    pub(super) fn client_role_marker_trait(&self) -> Option<TokenStream> {
        match self.owner_role() {
            OwnerRole::Publishes => Some(quote! { ::phoxal_bus::ClientReceiveContract }),
            OwnerRole::Consumes => Some(quote! { ::phoxal_bus::ClientPublishContract }),
            OwnerRole::Serves => None,
        }
    }

    pub(super) fn owner_role(&self) -> OwnerRole {
        match self {
            Self::State(_) | Self::Sample(_) | Self::Event(_) => OwnerRole::Publishes,
            Self::Setpoint(_)
            | Self::Stream {
                direction: StreamDirection::In,
                ..
            } => OwnerRole::Consumes,
            Self::Stream {
                direction: StreamDirection::Out,
                ..
            } => OwnerRole::Publishes,
            Self::Query { .. } => OwnerRole::Serves,
        }
    }

    pub(super) fn body_paths_mut(&mut self) -> Vec<&mut BodyPath> {
        match self {
            Self::State(body)
            | Self::Sample(body)
            | Self::Event(body)
            | Self::Setpoint(body)
            | Self::Stream { body, .. } => vec![body],
            Self::Query { request, response } => vec![request, response],
        }
    }
}

/// A resolved payload path. A catalogue section names a local sibling type and
/// the parser qualifies it against that section's module path.
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

/// One fully resolved family tree ready to emit.
///
/// The module name is also the leading wire-key segment and family identity.
pub(super) struct MaterializedTree {
    pub(super) module: Ident,
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
