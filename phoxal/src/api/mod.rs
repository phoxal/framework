//! The `robot` contract family: the surface a participant authors against.
//!
//! This is the robot family and only the robot family. The `runtime` and
//! `supervisor` families are host-tooling surfaces, reached through
//! `phoxal::runtime::api` and `phoxal::supervisor::api`.
//!
//! # The tree
//!
//! Payload structs, enums, implementations, and tests are ordinary Rust items
//! in the module that owns them. The declaration below owns this level of the
//! path; each child module owns its own. Every module in the tree is exactly
//! one of two things: a **branch**, which declares its child nodes with
//! `nodes!` and carries no endpoints, or a **leaf**, which declares its
//! endpoints with `endpoints!` beside the payloads and has no children. A
//! module cannot be both; the two declarations define the same items, so
//! trying is a duplicate-item compile error at the second invocation. A
//! node-level endpoint is spelled `self:` inside a leaf (`runtime/logs`,
//! `supervisor/snapshot`), which keeps "the key is the node path" without
//! letting a branch also carry endpoints. Walk the tree from `topics()`, bind
//! the dynamic segments as you go, and choose the side at the endpoint:
//!
//! ```ignore
//! let api = phoxal::api::topics();
//! let state = api.drive().state().client();
//! let joint = api.joint(&joint_id)?.state().client();
//! let frame = api.component(&instance)?.camera(&capability)?.frame().client();
//! ```
//!
//! The family is the first path segment of every key it declares: it names a
//! semantic namespace, not a revision. Compatibility is owned entirely by the
//! framework train version each participant binary embeds, so no key or body
//! carries a per-API version.
//!
//! Endpoint semantics are fixed by the declaration - `State`, `Sample`,
//! `Event`, `Stream`, `Setpoint`, or a bounded query. Source identity,
//! robot/capture time, ordered positions, loss, gaps, and terminal evidence
//! remain bus metadata and never become generated fields in a domain payload.

use crate::identity::ComponentInstanceId;
use crate::model::identity::JointId;

crate::nodes! {
    family Robot;

    component(instance: ComponentInstanceId);
    drive;
    frame;
    joint(joint: JointId);
    localize;
    map;
    motion;
    navigation;
    odometry;
    perception;
    safety;
    video;
}
