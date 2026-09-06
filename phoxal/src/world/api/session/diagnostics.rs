//! Bounded operational evidence that never controls pacing or scheduling.

use super::WorldInstanceId;

crate::endpoints! {
    self: Stream<WorldSessionDiagnosticsStream, Out>;
    current: Query<WorldSessionDiagnosticsCurrentRequest, WorldSessionDiagnosticsCurrentResponse>;
}

/// One positive completed running window.
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
pub struct ObservedWorldPacing {
    pub world_elapsed_ns: u64,
    pub host_elapsed_ns: u64,
    pub completed_transitions: u64,
}

impl ObservedWorldPacing {
    #[must_use]
    pub const fn is_valid(self) -> bool {
        self.world_elapsed_ns > 0 && self.host_elapsed_ns > 0 && self.completed_transitions > 0
    }
}

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
pub struct WorldSessionDiagnostics {
    pub revision: u64,
    pub pacing: Option<ObservedWorldPacing>,
    pub last_transition_age_ns: Option<u64>,
}

impl WorldSessionDiagnostics {
    /// Validate the bounded diagnostic window.
    ///
    /// # Errors
    ///
    /// Returns [`WorldSessionDiagnosticsError`] when a present pacing window
    /// contains no elapsed world time, host time, or completed transition.
    pub fn validate(self) -> Result<(), WorldSessionDiagnosticsError> {
        if self.pacing.is_some_and(ObservedWorldPacing::is_valid) || self.pacing.is_none() {
            Ok(())
        } else {
            Err(WorldSessionDiagnosticsError)
        }
    }
}

/// A diagnostics value whose pacing window cannot represent an observation.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
#[error("a world pacing window must contain positive world time, host time, and transitions")]
pub struct WorldSessionDiagnosticsError;

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
pub struct WorldSessionDiagnosticsStream {
    pub diagnostics: WorldSessionDiagnostics,
}

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
pub struct WorldSessionDiagnosticsCurrentRequest {
    pub instance: WorldInstanceId,
}

/// Identity binding for a long-lived diagnostics subscription.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Eq, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldSessionDiagnosticsSubscriptionRequest {
    pub instance: WorldInstanceId,
}

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
pub struct WorldSessionDiagnosticsCurrentResponse {
    pub diagnostics: WorldSessionDiagnostics,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pacing_windows_are_absent_or_strictly_positive() {
        assert!(
            WorldSessionDiagnostics {
                revision: 0,
                pacing: None,
                last_transition_age_ns: None,
            }
            .validate()
            .is_ok()
        );
        assert!(
            WorldSessionDiagnostics {
                revision: 1,
                pacing: Some(ObservedWorldPacing {
                    world_elapsed_ns: 1,
                    host_elapsed_ns: 1,
                    completed_transitions: 1,
                }),
                last_transition_age_ns: Some(0),
            }
            .validate()
            .is_ok()
        );
        for invalid in [
            ObservedWorldPacing {
                world_elapsed_ns: 0,
                host_elapsed_ns: 1,
                completed_transitions: 1,
            },
            ObservedWorldPacing {
                world_elapsed_ns: 1,
                host_elapsed_ns: 0,
                completed_transitions: 1,
            },
            ObservedWorldPacing {
                world_elapsed_ns: 1,
                host_elapsed_ns: 1,
                completed_transitions: 0,
            },
        ] {
            assert!(
                WorldSessionDiagnostics {
                    revision: 2,
                    pacing: Some(invalid),
                    last_transition_age_ns: Some(0),
                }
                .validate()
                .is_err()
            );
        }
    }
}
