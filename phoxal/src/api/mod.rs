//! The `robot` contract family: the surface a participant authors against.
//!
//! This is the robot family and only the robot family. The `runtime` and
//! `supervisor` families are host-tooling surfaces, reached through
//! [`crate::runtime::api`] and [`crate::supervisor::api`].
//!
//! # The tree
//!
//! Payload structs, enums, implementations, and tests are ordinary Rust items
//! in the module that owns them. The declaration below owns this level of the
//! path; each child module owns its own. Walk it from [`topics()`], bind the
//! dynamic segments as you go, and choose the side at the endpoint:
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
