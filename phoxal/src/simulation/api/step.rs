//! Passive progress for one attached Live simulation controller.

crate::endpoints! {
    self: Event<StepEvent>;
}

/// Notification that one native world transition completed.
///
/// The exact monotonic [`crate::bus::RobotInstant`] is carried by standard
/// message metadata. The execution bus supplies [`crate::identity::ExecutionId`], and
/// the active supervisor attachment supplies the world identity. This body
/// therefore carries only the world-absolute completed transition index.
///
/// This event is producer-ordered progress. It is not a receiver-side
/// transaction, an observation fence, or a participant scheduling trigger.
#[derive(
    phoxal_macros::DescribeWire,
    Clone,
    Copy,
    Debug,
    Eq,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct StepEvent {
    /// The world-absolute native step that just completed.
    pub index: u64,
}
