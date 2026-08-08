//! `phoxal_api_tree!` - the single API layer.
//!
//! Grammar. The body of a `version` is a tree of **nodes**. A node is either
//! static (`name { … }`) or dynamic (`name(var) { … }`); it can be nested to any
//! depth and may hold any mix of types (`struct`/`enum`), `topic` declarations,
//! and child nodes. Every topic declares a **role**: `topic name: command Body;`
//! (a setpoint the owner subscribes), `topic name: stream Body;` (ordered chunks
//! the owner subscribes), `topic name: state Body;` (telemetry the owner
//! publishes), `topic name: event Body;` (an owner-published state-temporal
//! event with ordered stream delivery), or `topic name: query Request => Response;`
//! (request/response). `command`, `stream`, `state`, and `event` are all pub/sub
//! on the wire; the
//! role drives the side-branded builders: the public client builder
//! (`api::topic::client()...`) and the owner builder (`api::topic::owner()...`)
//! return side-branded topics (`Publish`/`Subscribe`/`AskQuery`/`ServeQuery`), so
//! taking the wrong side does not compile. The role is also emitted as a `ROLE`
//! const on each body. A topic's key and dynamism are
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
//!                 enum Command { Position(f32), Velocity(f32), Torque(f32), Stop }
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
//! bodies (plain serde types, with no envelope wrapper) and their
//! `ContractBody` impls, plus an api-local `topic` builder module.
//!
//! **Wire identity is the version-qualified key, not a transitive-shape hash.**
//! The version is folded into `ContractBody::TOPIC`: a contract's
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
//! `Node::expand_builder_module`) is intentionally left as a direct `super::`: it
//! names this builder's immediate enclosing module, which is by construction
//! always exactly one hop away regardless of depth, so it is not part of the
//! depth-independence scheme above.
//!
//! # No path-based rename
//!
//! There is no attribute that decouples a node/leaf's wire key segment from its
//! Rust identifier, and there is deliberately no way to add one: identity stays
//! on a single axis, where the version-qualified Rust path IS the wire key
//! (`v0.1::drive::Target` <-> `v0.1/drive/target`). A path that needs different
//! wording is declared explicitly in a new child revision, which states the
//! contract change rather than hiding it.

mod bodies;
mod builders;
mod grammar;
mod manifest;
mod model;

use proc_macro2::TokenStream;
use quote::quote;
use syn::Ident;

use grammar::{PROTOCOL_HAS_NO_DELTAS, VERSION_HAS_NO_PARENT};
use manifest::ManifestVersion;
use model::{MaterializedTree, Node, Protocol, Version};

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
            Node::reject_delta_forms(&protocol.nodes, &[], PROTOCOL_HAS_NO_DELTAS)?;
            let tree = MaterializedTree {
                module: protocol.name.clone(),
                doc: format!(
                    "Protocol tree `{id}` - relative wire keys, developer-owned schema-tagged \
                     bodies."
                ),
                id,
                nodes: protocol.nodes.clone(),
            };
            manifest_trees.push(ManifestVersion::of(&tree));
            out.extend(tree.expand());
        }
        let manifest = ManifestVersion::expand_manifest(&manifest_trees);
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
            let nodes = match &version.parent {
                Some(parent) => {
                    let base = materialized.get(&parent.to_string()).ok_or_else(|| {
                        syn::Error::new_spanned(
                            parent,
                            "an `extends` parent must be a concrete revision declared earlier",
                        )
                    })?;
                    version.materialize_from(base, parent)?
                }
                None => {
                    Node::reject_delta_forms(
                        &version.nodes,
                        &version.removals,
                        VERSION_HAS_NO_PARENT,
                    )?;
                    version.nodes.clone()
                }
            };
            let concrete = MaterializedTree {
                module: version.name.clone(),
                id: version.wire_id.clone(),
                doc: format!(
                    "Concrete API revision `{}` - version-local wire bodies + topics.",
                    version.wire_id
                ),
                nodes,
            };
            manifest_versions.push(ManifestVersion::of(&concrete));
            out.extend(concrete.expand());
            materialized.insert(name, concrete.nodes);
        }
        if !materialized.contains_key(&latest.to_string()) {
            return Err(syn::Error::new_spanned(
                latest,
                "`latest` must name a declared concrete API revision",
            ));
        }
        let manifest = ManifestVersion::expand_manifest(&manifest_versions);
        Ok(quote! {
            #manifest
            #out
            /// The concrete API revision selected by this framework train.
            pub use #latest as latest;
        })
    }
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
            "TOPIC must fold the version into the wire key: {expanded}"
        );
        assert!(
            expanded.contains("const NAME : & 'static str = \"v0.1::drive::Target\""),
            "NAME must be the version-qualified type path: {expanded}"
        );
        assert!(
            expanded.contains("const VERSION : & 'static str = \"v0.1\""),
            "VERSION must be the bare version, split from CONTRACT: {expanded}"
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
    fn a_protocol_tree_after_a_version_tree_is_rejected_as_mode_mixing() {
        for tokens in [
            quote! {
                version v0_1 { drive { struct Target { value: u8 } } }
                protocol supervisor { connect { struct Hello { token: String } } }
                latest v0_1;
            },
            quote! {
                version v0_1 { drive { struct Target { value: u8 } } }
                latest v0_1;
                protocol supervisor { connect { struct Hello { token: String } } }
            },
        ] {
            let error = expand(tokens).expect_err("mixing the two modes must be rejected");
            assert!(
                error.to_string().contains("stands alone"),
                "unexpected error: {error}"
            );
        }
    }

    #[test]
    fn a_protocol_tree_nested_inside_a_version_tree_is_rejected_as_mode_mixing() {
        for tokens in [
            // Directly in a version body.
            quote! {
                version v0_1 {
                    protocol supervisor { connect { struct Hello { token: String } } }
                }
                latest v0_1;
            },
            // One level down, inside a node body.
            quote! {
                version v0_1 {
                    drive {
                        protocol supervisor { connect { struct Hello { token: String } } }
                    }
                }
                latest v0_1;
            },
        ] {
            let error = expand(tokens).expect_err("a nested protocol tree must be rejected");
            assert!(
                error.to_string().contains("stands alone"),
                "unexpected error: {error}"
            );
        }
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
            "manifest family is the version-qualified contract identity: {expanded}"
        );
        assert!(
            expanded.contains("topic : \"v0.2/sample/body\""),
            "manifest topic is the version-qualified wire key: {expanded}"
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
