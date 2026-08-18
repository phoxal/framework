//! The dynamic endpoint tree: the addressed-endpoint value, the typed dynamic
//! segment, and the two declarations that build the tree out of ordinary Rust
//! modules.
//!
//! # The shape
//!
//! A family is a Rust module tree. Each module is either a **branch** or a
//! **leaf**:
//!
//! - a branch declares its child modules with `nodes!`, each child
//!   being a static segment (`drive;`) or a static segment followed by one
//!   typed dynamic segment (`joint(joint: JointId);`);
//! - a leaf declares its endpoints with `endpoints!`, beside the
//!   payload types they carry.
//!
//! Both declarations define a `Path` in their own module, so a module that
//! declared both would fail to compile with a duplicate `Path`. That is the
//! intended diagnostic: a module is a branch or a leaf, never both.
//!
//! Walking `Path` accumulates the concrete key; the leaf method returns a
//! [`BoundEndpoint`], and the side is chosen last, at the endpoint, with
//! [`BoundEndpoint::client`] or [`BoundEndpoint::owner`]. There is one path
//! tree, not one per side, and no complete topic string is ever authored: a
//! static segment is spelled by its child module's identifier, a dynamic
//! placeholder by its declared variable's identifier, and the leading segment
//! by [`Family::ID`](crate::bus::Family::ID).
//!
//! # The one non-obvious mechanism
//!
//! The family marker reaches every declaration without being authored twice.
//! The family root states it once (`family Robot;`), and every `nodes!` and
//! `endpoints!` expansion emits `pub(crate) use super::Family;` at its own
//! level, so `super::Family` resolves at any depth by induction.
//!
//! # Compatibility records
//!
//! The same declarations emit the compatibility evidence. A branch's
//! `contract_records` recurses into its children, appending its own segment and
//! spelling a dynamic child as `{variable}`; a leaf's pushes one record per
//! endpoint. Concrete keys and `{placeholder}` templates are therefore built by
//! the same structure and cannot drift.

use std::marker::PhantomData;

use crate::bus::contract::{Endpoint, EndpointSemantics};
use crate::bus::topic::{KeySegment, KeySegmentError, Topic};

/// One addressed endpoint: the concrete family-rooted key a walk of the tree
/// accumulated, bound to the endpoint type at that leaf.
///
/// It is produced only by the tree, and consumed by choosing a side. Holding
/// one is holding the fact that this key carries this endpoint - and nothing
/// yet about which side of it the holder may take.
pub struct BoundEndpoint<E: Endpoint> {
    key: String,
    _endpoint: PhantomData<fn() -> E>,
}

impl<E: Endpoint> BoundEndpoint<E> {
    /// Bind an accumulated concrete key to its endpoint.
    ///
    /// Crate-private, and the only constructor: the declarations in
    /// `phoxal::api`, `phoxal::runtime::api` and `phoxal::supervisor::api` are
    /// its callers, together with the bus's own unit tests, which need
    /// stand-in endpoints without a generated tree.
    pub(crate) fn new(key: String) -> Self {
        BoundEndpoint {
            key,
            _endpoint: PhantomData,
        }
    }

    /// The concrete family-rooted key this endpoint is bound at.
    #[must_use]
    pub fn key(&self) -> &str {
        &self.key
    }

    /// Take the external client's side of this endpoint.
    #[must_use]
    pub fn client(self) -> Topic<<E::Semantics as EndpointSemantics>::Client<E>> {
        Topic::new(self.key)
    }

    /// Take the endpoint owner's side of this endpoint.
    #[must_use]
    pub fn owner(self) -> Topic<<E::Semantics as EndpointSemantics>::Owner<E>> {
        Topic::new(self.key)
    }
}

/// A value that may stand as one dynamic segment of a topic key.
///
/// Deliberately not a blanket implementation over [`std::fmt::Display`]: a
/// dynamic segment is a model identity, so the set of things that may fill one
/// is a short, deliberate list rather than anything that can print itself.
///
/// Every implementation is fallible even where the identity type already proves
/// its own validity. One signature is simpler than two, every call site already
/// writes `?`, and the alternative - an infallible variant beside it - would
/// make the tree's dynamic binders a choice rather than a rule.
pub trait TopicSegment {
    /// Narrow this value to one concrete key segment.
    ///
    /// # Errors
    ///
    /// Returns [`KeySegmentError`] when the value is not one concrete,
    /// wildcard-free key segment.
    fn segment(&self) -> Result<KeySegment, KeySegmentError>;
}

impl TopicSegment for KeySegment {
    fn segment(&self) -> Result<KeySegment, KeySegmentError> {
        Ok(self.clone())
    }
}

/// Declare this module's child nodes, and - at a family root - the family
/// itself.
///
/// ```ignore
/// crate::nodes! {
///     family Robot;                              // family roots only
///     component(instance: ComponentInstanceId);  // static + typed dynamic segment
///     drive;                                     // static segment
/// }
/// ```
///
/// Each declaration owns one child module, its literal segment, its dynamic
/// placeholder name and type, the builder method that binds it, and the
/// recursive compatibility aggregation into it.
macro_rules! nodes {
    // A family root: it states the family once, and is the only node with no
    // parent to inherit a key prefix from.
    (
        family $family:ident ;
        $( $node:ident $( ( $variable:ident : $segment:ty ) )? ; )+
    ) => {
        /// This family's marker. Every node and leaf below inherits it through
        /// its own `pub(crate) use super::Family;`.
        pub(crate) type Family = crate::bus::$family;

        crate::bus::tree::path_node!();

        impl Path {
            fn __root() -> Self {
                Path {
                    key: ::std::string::ToString::to_string(
                        <Family as crate::bus::Family>::ID,
                    ),
                }
            }
        }

        #[doc = ::core::concat!("Begin a path through the `", ::core::stringify!($family), "` family, rooted at its own leading key segment.")]
        #[must_use]
        pub fn topics() -> Path {
            Path::__root()
        }

        $( pub mod $node; )+
        $( crate::bus::tree::node_child!( $node $( ( $variable : $segment ) )? ); )+

        /// Every endpoint record this family declares.
        pub(crate) fn contract_records(
            out: &mut ::std::vec::Vec<crate::__compat::surface::ContractRecord>,
        ) {
            let root = <Family as crate::bus::Family>::ID;
            $( crate::bus::tree::node_record!(root, out, $node $( ( $variable : $segment ) )? ); )+
        }
    };

    // An ordinary branch.
    ( $( $node:ident $( ( $variable:ident : $segment:ty ) )? ; )+ ) => {
        pub(crate) use super::Family;

        crate::bus::tree::path_node!();

        crate::bus::tree::path_child!();

        $( pub mod $node; )+
        $( crate::bus::tree::node_child!( $node $( ( $variable : $segment ) )? ); )+

        /// Every endpoint record below this node, under `prefix`.
        pub(crate) fn contract_records(
            prefix: &str,
            out: &mut ::std::vec::Vec<crate::__compat::surface::ContractRecord>,
        ) {
            $( crate::bus::tree::node_record!(prefix, out, $node $( ( $variable : $segment ) )? ); )+
        }
    };
}

/// Declare this module's endpoints, beside the payload types they carry.
///
/// ```ignore
/// crate::endpoints! {
///     target: Setpoint<Target>;
///     state: State<State>;
/// }
/// ```
///
/// The leaf name is the final key segment; `self:` binds the endpoint to the
/// node path itself, so a key such as `runtime/logs` carries no invented leaf
/// segment. The declaration emits the sealed endpoint typing, the semantics,
/// the query response linkage, the bound-leaf builder, the side branding, and
/// the compatibility record.
macro_rules! endpoints {
    ( $( $leaf:tt : $semantics:tt < $( $body:tt ),+ $(,)? > ; )+ ) => {
        pub(crate) use super::Family;

        crate::bus::tree::path_node!();

        crate::bus::tree::path_child!();

        $( crate::bus::tree::endpoint!( $leaf, $semantics, $( $body ),+ ); )+

        /// Every endpoint record this leaf declares, under `prefix`.
        pub(crate) fn contract_records(
            prefix: &str,
            out: &mut ::std::vec::Vec<crate::__compat::surface::ContractRecord>,
        ) {
            $( crate::bus::tree::endpoint_record!(prefix, out, $leaf, $semantics, $( $body ),+ ); )+
        }
    };
}

/// This module's node in the tree: the key accumulated down to here.
///
/// Emitted by both declarations, which is what makes a module that tried to be
/// a branch and a leaf at once a duplicate-`Path` compile error.
macro_rules! path_node {
    () => {
        /// This node of the family's dynamic topic path, carrying the concrete
        /// key accumulated from the family root down to here.
        pub struct Path {
            key: ::std::string::String,
        }
    };
}

/// The constructor a parent node uses to descend into this one.
macro_rules! path_child {
    () => {
        impl Path {
            pub(crate) fn __child(parent: &str, segment: &str) -> Self {
                Path {
                    key: ::std::format!("{parent}/{segment}"),
                }
            }
        }
    };
}

/// The builder method that descends into one child node.
macro_rules! node_child {
    ( $node:ident ) => {
        impl Path {
            #[doc = ::core::concat!("Descend into the `", ::core::stringify!($node), "` node.")]
            #[must_use]
            pub fn $node(&self) -> $node::Path {
                $node::Path::__child(&self.key, ::core::stringify!($node))
            }
        }
    };
    ( $node:ident ( $variable:ident : $segment:ty ) ) => {
        impl Path {
            #[doc = ::core::concat!("Descend into the `", ::core::stringify!($node), "/{", ::core::stringify!($variable), "}` node, binding `", ::core::stringify!($variable), "` as its dynamic segment.")]
            ///
            /// # Errors
            ///
            /// Returns [`KeySegmentError`](crate::bus::KeySegmentError) when the
            /// bound value is not one concrete key segment.
            pub fn $node(
                &self,
                $variable: &$segment,
            ) -> ::core::result::Result<$node::Path, crate::bus::KeySegmentError> {
                let bound = crate::bus::TopicSegment::segment($variable)?;
                ::core::result::Result::Ok($node::Path::__child(
                    &self.key,
                    &::std::format!("{}/{}", ::core::stringify!($node), bound),
                ))
            }
        }
    };
}

/// Recurse the compatibility aggregation into one child node, spelling a
/// dynamic segment as its `{variable}` template.
macro_rules! node_record {
    ( $prefix:expr, $out:expr, $node:ident ) => {
        $node::contract_records(
            &::std::format!("{}/{}", $prefix, ::core::stringify!($node)),
            $out,
        );
    };
    ( $prefix:expr, $out:expr, $node:ident ( $variable:ident : $segment:ty ) ) => {
        $node::contract_records(
            &::std::format!(
                "{}/{}/{{{}}}",
                $prefix,
                ::core::stringify!($node),
                ::core::stringify!($variable)
            ),
            $out,
        );
    };
}

/// One endpoint declaration: map the authored semantic to its semantics type,
/// and add the query response linkage where there is one.
macro_rules! endpoint {
    ( $leaf:tt, State, $body:tt ) => {
        crate::bus::tree::endpoint_declare!($leaf, crate::bus::State, $body);
    };
    ( $leaf:tt, Sample, $body:tt ) => {
        crate::bus::tree::endpoint_declare!($leaf, crate::bus::Sample, $body);
    };
    ( $leaf:tt, Event, $body:tt ) => {
        crate::bus::tree::endpoint_declare!($leaf, crate::bus::Event, $body);
    };
    ( $leaf:tt, Setpoint, $body:tt ) => {
        crate::bus::tree::endpoint_declare!($leaf, crate::bus::Setpoint, $body);
    };
    ( $leaf:tt, WorldClock, $body:tt ) => {
        crate::bus::tree::endpoint_declare!($leaf, crate::bus::WorldClock, $body);
    };
    ( $leaf:tt, Stream, $body:tt, In ) => {
        crate::bus::tree::endpoint_declare!($leaf, crate::bus::Stream<crate::bus::In>, $body);
    };
    ( $leaf:tt, Stream, $body:tt, Out ) => {
        crate::bus::tree::endpoint_declare!($leaf, crate::bus::Stream<crate::bus::Out>, $body);
    };
    ( $leaf:tt, Query, $request:tt, $response:tt ) => {
        crate::bus::tree::endpoint_declare!($leaf, crate::bus::Query, $request);

        impl crate::bus::QueryEndpoint for $request {
            type Response = $response;
        }
    };
}

/// The sealed endpoint typing plus the builder that binds this leaf.
macro_rules! endpoint_declare {
    ( self, $semantics:ty, $body:tt ) => {
        crate::bus::tree::endpoint_bind!($semantics, $body);

        impl Path {
            /// Take the external client's side of this node's own endpoint.
            #[must_use]
            pub fn client(
                self,
            ) -> crate::bus::Topic<<$semantics as crate::bus::EndpointSemantics>::Client<$body>>
            {
                crate::bus::BoundEndpoint::<$body>::new(self.key).client()
            }

            /// Take the endpoint owner's side of this node's own endpoint.
            #[must_use]
            pub fn owner(
                self,
            ) -> crate::bus::Topic<<$semantics as crate::bus::EndpointSemantics>::Owner<$body>>
            {
                crate::bus::BoundEndpoint::<$body>::new(self.key).owner()
            }
        }
    };
    ( $leaf:ident, $semantics:ty, $body:tt ) => {
        crate::bus::tree::endpoint_bind!($semantics, $body);

        impl Path {
            #[doc = ::core::concat!("Bind the `", ::core::stringify!($leaf), "` endpoint of this node.")]
            #[must_use]
            pub fn $leaf(&self) -> crate::bus::BoundEndpoint<$body> {
                crate::bus::BoundEndpoint::new(::std::format!(
                    "{}/{}",
                    self.key,
                    ::core::stringify!($leaf)
                ))
            }
        }
    };
}

/// Attach the family and the semantics to the payload type, sealing it as an
/// endpoint.
macro_rules! endpoint_bind {
    ( $semantics:ty, $body:tt ) => {
        impl crate::bus::contract::sealed::Endpoint for $body {}

        impl crate::bus::Endpoint for $body {
            type Family = self::Family;
            type Semantics = $semantics;
        }
    };
}

/// The key template one endpoint contributes below `prefix`.
macro_rules! endpoint_key {
    ( $prefix:expr, self ) => {
        ::std::string::String::from($prefix)
    };
    ( $prefix:expr, $leaf:ident ) => {
        ::std::format!("{}/{}", $prefix, ::core::stringify!($leaf))
    };
}

/// One endpoint's compatibility record.
macro_rules! endpoint_record {
    ( $prefix:expr, $out:expr, $leaf:tt, Stream, $body:tt, $direction:tt ) => {
        crate::bus::tree::endpoint_topic_record!(
            $prefix,
            $out,
            $leaf,
            crate::bus::Stream<crate::bus::$direction>,
            $body
        );
    };
    ( $prefix:expr, $out:expr, $leaf:tt, Query, $request:tt, $response:tt ) => {
        $out.push(crate::__compat::surface::ContractRecord::query(
            <self::Family as crate::bus::Family>::ID,
            crate::bus::tree::endpoint_key!($prefix, $leaf),
            <crate::bus::Query as crate::bus::EndpointSemantics>::KIND.as_str(),
            <crate::bus::Query as crate::bus::EndpointSemantics>::DELIVERY.as_str(),
            <$request as crate::__compat::wire::DescribeWire>::wire_schema(),
            <$response as crate::__compat::wire::DescribeWire>::wire_schema(),
        ));
    };
    ( $prefix:expr, $out:expr, $leaf:tt, $semantics:tt, $body:tt ) => {
        crate::bus::tree::endpoint_topic_record!(
            $prefix,
            $out,
            $leaf,
            crate::bus::$semantics,
            $body
        );
    };
}

/// One pub/sub endpoint's compatibility record.
macro_rules! endpoint_topic_record {
    ( $prefix:expr, $out:expr, $leaf:tt, $semantics:ty, $body:tt ) => {
        $out.push(crate::__compat::surface::ContractRecord::topic(
            <self::Family as crate::bus::Family>::ID,
            crate::bus::tree::endpoint_key!($prefix, $leaf),
            <$semantics as crate::bus::EndpointSemantics>::KIND.as_str(),
            <$semantics as crate::bus::EndpointSemantics>::DELIVERY.as_str(),
            <$body as crate::__compat::wire::DescribeWire>::wire_schema(),
        ));
    };
}

pub(crate) use endpoint;
pub(crate) use endpoint_bind;
pub(crate) use endpoint_declare;
pub(crate) use endpoint_key;
pub(crate) use endpoint_record;
pub(crate) use endpoint_topic_record;
pub(crate) use endpoints;
pub(crate) use node_child;
pub(crate) use node_record;
pub(crate) use nodes;
pub(crate) use path_child;
pub(crate) use path_node;
