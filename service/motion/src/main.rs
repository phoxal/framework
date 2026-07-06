//! `motion` - the official body-twist arbiter.
//!
//! This participant is the single authority that writes `drive/target`. It subscribes
//! to `motion/manual`, `follow/target`, and `safety/authorization`, chooses
//! between the manual and follow candidates under the current safety envelope, and
//! publishes both the final `drive/target` and a `motion/state` arbitration
//! record.
//!
//! Safety is applied first and conservatively. A missing, stale, or expired
//! authorization forces a zero target, and an `EmergencyStop` decision overrides
//! every candidate. A fresh manual command wins over follow and is clamped to the
//! approved-motion envelope - including the limited escape envelope under a
//! protective `Stop`, where follow is otherwise blocked. The selected twist is
//! always clamped to the authorized min/max per axis, and a malformed envelope
//! (non-finite or inverted bounds) clamps that axis to zero.

mod arbitration;

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

use crate::arbitration::{Timed, arbitrate};

#[derive(phoxal::Service)]
#[phoxal(id = "motion", api = y2026_1)]
struct Motion {
    // Runtime-private typed state.
    last_manual: Option<Timed<api::motion::ManualCommand>>,
    last_follow: Option<Timed<api::follow::Target>>,
    last_safety: Option<Timed<api::safety::SafetyAuthorization>>,
    // Handles.
    manual: Subscriber<api::motion::ManualCommand>,
    follow: Subscriber<api::follow::Target>,
    safety: Subscriber<api::safety::SafetyAuthorization>,
    drive: Publisher<api::drive::Target>,
    state: Publisher<api::motion::State>,
}

#[phoxal::behavior]
impl Motion {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        // Owner opt-in (plan #00 L2): the runner-minted capability that the
        // owner (`internal`) topic builder requires.
        let cap = ctx.owner_capability();
        // Motion OWNS the `motion` node (its manual input + its state telemetry) ->
        // owner (`internal`) builder. It CONSUMES `follow/target` and
        // `safety/authorization` and CLIENT-publishes `drive/target` (a command it
        // sends to the drive owner) -> public builder.
        let manual = ctx
            .subscribe(api::topic::internal::new(cap).motion().manual())
            .subscriber()
            .await?;
        let follow = ctx
            .subscribe(api::topic::new().follow().target())
            .subscriber()
            .await?;
        let safety = ctx
            .subscribe(api::topic::new().safety().authorization())
            .subscriber()
            .await?;
        let drive = ctx.publisher(api::topic::new().drive().target()).await?;
        let state = ctx
            .publisher(api::topic::internal::new(cap).motion().state())
            .await?;

        Ok(Self {
            last_manual: None,
            last_follow: None,
            last_safety: None,
            manual,
            follow,
            safety,
            drive,
            state,
        })
    }

    #[step(hz = 20)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        while let Some(received) = self.manual.try_recv() {
            self.last_manual = Some(Timed {
                body: received.body,
                produced_at_ns: received.metadata.produced_at_ns,
            });
        }
        while let Some(received) = self.follow.try_recv() {
            self.last_follow = Some(Timed {
                body: received.body,
                produced_at_ns: received.metadata.produced_at_ns,
            });
        }
        while let Some(received) = self.safety.try_recv() {
            self.last_safety = Some(Timed {
                body: received.body,
                produced_at_ns: received.metadata.produced_at_ns,
            });
        }

        let arbitration = arbitrate(
            self.last_manual.as_ref(),
            self.last_follow.as_ref(),
            self.last_safety.as_ref(),
            step.time().time_ns(),
        );
        let drive_target = drive_target_from(arbitration.selected.clone());
        self.drive.publish_at(step.time(), drive_target).await?;
        self.state
            .publish_at(
                step.time(),
                api::motion::State {
                    active_source: arbitration.active_source,
                    selected: Some(arbitration.selected),
                    reason: arbitration.reason,
                },
            )
            .await?;
        Ok(())
    }
}

fn drive_target_from(target: api::motion::Target) -> api::drive::Target {
    api::drive::Target {
        linear_x_mps: target.linear_x_mps,
        angular_z_radps: target.angular_z_radps,
        curvature_limit_radpm: target.curvature_limit_radpm,
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Motion>()
}

#[cfg(test)]
mod tests {
    use phoxal_api::ContractBody;

    use super::{Motion, api, drive_target_from};
    use crate::arbitration::{
        FOLLOW_STALE_NS, SAFETY_STALE_NS, Timed, arbitrate, clamp_to_constraint, zero_motion_target,
    };

    const NOW_NS: u64 = 1_000_000_000;

    #[test]
    fn missing_safety_authorization_forces_stop() {
        let arbitration = arbitrate(None, None, None, NOW_NS);

        assert_eq!(
            arbitration.active_source,
            Some(api::motion::MotionSource::MissionStop)
        );
        assert_eq!(arbitration.selected, zero_motion_target());
        assert_eq!(
            arbitration.reason,
            Some(api::motion::MotionReason::SafetyAuthorizationUnavailable)
        );
    }

    #[test]
    fn expired_or_stale_safety_authorization_forces_stop() {
        let expired = timed(
            NOW_NS,
            safety_authorization_with_expiry(api::safety::SafetyDecision::Allow, Some(NOW_NS - 1)),
        );
        let stale = timed(
            NOW_NS - SAFETY_STALE_NS - 1,
            safety_authorization(api::safety::SafetyDecision::Allow),
        );

        assert_eq!(
            arbitrate(None, None, Some(&expired), NOW_NS).reason,
            Some(api::motion::MotionReason::SafetyAuthorizationUnavailable)
        );
        assert_eq!(
            arbitrate(None, None, Some(&stale), NOW_NS).reason,
            Some(api::motion::MotionReason::SafetyAuthorizationUnavailable)
        );
    }

    #[test]
    fn emergency_stop_overrides_every_candidate() {
        let manual = timed(NOW_NS, manual_command(-0.2, 0.4));
        let follow = timed(NOW_NS, follow_target(0.5, 0.0));
        let safety = timed(
            NOW_NS,
            safety_authorization(api::safety::SafetyDecision::EmergencyStop),
        );

        let arbitration = arbitrate(Some(&manual), Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(
            arbitration.active_source,
            Some(api::motion::MotionSource::EmergencyStop)
        );
        assert_eq!(arbitration.selected, zero_motion_target());
        assert_eq!(
            arbitration.reason,
            Some(api::motion::MotionReason::SafetyEmergencyStop)
        );
    }

    #[test]
    fn fresh_manual_overrides_follow_and_is_clamped() {
        let manual = timed(NOW_NS, manual_command(5.0, -5.0));
        let follow = timed(NOW_NS, follow_target(0.5, 0.2));
        let safety = timed(
            NOW_NS,
            safety_authorization_with_limits(api::safety::SafetyDecision::Slow, 0.10, 0.30),
        );

        let arbitration = arbitrate(Some(&manual), Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(
            arbitration.active_source,
            Some(api::motion::MotionSource::Manual)
        );
        assert_eq!(arbitration.selected.linear_x_mps, 0.10);
        assert_eq!(arbitration.selected.angular_z_radps, -0.30);
        assert_eq!(
            arbitration.reason,
            Some(api::motion::MotionReason::SafetyConstrained(
                api::motion::SafetyDecision::Slow
            ))
        );
    }

    #[test]
    fn stale_manual_is_ignored_for_fresh_follow() {
        let manual = timed(NOW_NS - 1_000_000_000, manual_command(0.1, 0.0));
        let follow = timed(NOW_NS, follow_target(0.5, 0.2));
        let safety = timed(
            NOW_NS,
            safety_authorization(api::safety::SafetyDecision::Allow),
        );

        let arbitration = arbitrate(Some(&manual), Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(
            arbitration.active_source,
            Some(api::motion::MotionSource::Follow)
        );
        assert_eq!(arbitration.selected.linear_x_mps, 0.5);
        assert_eq!(arbitration.selected.angular_z_radps, 0.2);
        assert_eq!(arbitration.reason, None);
    }

    #[test]
    fn stale_or_missing_follow_stops_when_no_manual_candidate() {
        let safety = timed(
            NOW_NS,
            safety_authorization(api::safety::SafetyDecision::Allow),
        );
        let stale_follow = timed(NOW_NS - FOLLOW_STALE_NS - 1, follow_target(0.5, 0.2));

        let stale = arbitrate(None, Some(&stale_follow), Some(&safety), NOW_NS);
        assert_eq!(stale.active_source, None);
        assert_eq!(stale.selected, zero_motion_target());
        assert_eq!(
            stale.reason,
            Some(api::motion::MotionReason::FollowTargetStale)
        );

        let missing = arbitrate(None, None, Some(&safety), NOW_NS);
        assert_eq!(missing.active_source, None);
        assert_eq!(missing.selected, zero_motion_target());
        assert_eq!(
            missing.reason,
            Some(api::motion::MotionReason::NoFollowTarget)
        );
    }

    #[test]
    fn protective_stop_allows_manual_escape_envelope() {
        let manual = timed(NOW_NS, manual_command(-0.30, 1.5));
        let safety = timed(
            NOW_NS,
            safety_authorization_with_escape(api::safety::SafetyDecision::Stop, 0.15, 0.60),
        );

        let arbitration = arbitrate(Some(&manual), None, Some(&safety), NOW_NS);

        assert_eq!(
            arbitration.active_source,
            Some(api::motion::MotionSource::Manual)
        );
        assert_eq!(arbitration.selected.linear_x_mps, -0.15);
        assert_eq!(arbitration.selected.angular_z_radps, 0.60);
        assert_eq!(
            arbitration.reason,
            Some(api::motion::MotionReason::ManualEscapeUnderStop)
        );
    }

    #[test]
    fn protective_stop_blocks_follow() {
        let follow = timed(NOW_NS, follow_target(0.50, 0.20));
        let safety = timed(
            NOW_NS,
            safety_authorization_with_escape(api::safety::SafetyDecision::Stop, 0.15, 0.60),
        );

        let arbitration = arbitrate(None, Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(
            arbitration.active_source,
            Some(api::motion::MotionSource::MissionStop)
        );
        assert_eq!(arbitration.selected, zero_motion_target());
        assert_eq!(
            arbitration.reason,
            Some(api::motion::MotionReason::SafetyConstrained(
                api::motion::SafetyDecision::Stop
            ))
        );
    }

    #[test]
    fn drive_target_preserves_selected_motion_target() {
        let motion = api::motion::Target {
            linear_x_mps: 0.2,
            angular_z_radps: -0.3,
            curvature_limit_radpm: Some(2.0),
        };

        let drive = drive_target_from(motion);

        assert_eq!(drive.linear_x_mps, 0.2);
        assert_eq!(drive.angular_z_radps, -0.3);
        assert_eq!(drive.curvature_limit_radpm, Some(2.0));
    }

    #[test]
    fn clamp_handles_invalid_constraints_conservatively() {
        assert_eq!(
            clamp_to_constraint(
                1.0,
                &api::safety::Constraint {
                    min: 2.0,
                    max: -1.0
                }
            ),
            0.0
        );
        assert_eq!(
            clamp_to_constraint(
                f64::NAN,
                &api::safety::Constraint {
                    min: -1.0,
                    max: 1.0
                }
            ),
            0.0
        );
    }

    #[test]
    fn malformed_safety_envelope_zeros_both_axes() {
        let follow = timed(NOW_NS, follow_target(0.5, 0.2));
        let safety = timed(NOW_NS, malformed_safety_authorization());

        let arbitration = arbitrate(None, Some(&follow), Some(&safety), NOW_NS);

        assert_eq!(
            arbitration.active_source,
            Some(api::motion::MotionSource::Follow)
        );
        assert_eq!(arbitration.selected.linear_x_mps, 0.0);
        assert_eq!(arbitration.selected.angular_z_radps, 0.0);
    }

    #[test]
    fn emit_apis_reports_motion_contracts() {
        let json = phoxal::participant::emit_apis_json::<Motion>();
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["artifact"]["id"], "motion");
        assert_eq!(value["api_version"], "y2026_1");

        let contracts = value["required_contracts"].as_array().unwrap();
        assert_contract::<api::motion::ManualCommand>(contracts, "subscribe");
        assert_contract::<api::follow::Target>(contracts, "subscribe");
        assert_contract::<api::safety::SafetyAuthorization>(contracts, "subscribe");
        assert_contract::<api::drive::Target>(contracts, "publish");
        assert_contract::<api::motion::State>(contracts, "publish");
    }

    fn assert_contract<B>(contracts: &[serde_json::Value], direction: &str)
    where
        B: ContractBody,
    {
        assert!(contracts.iter().any(|c| {
            c["family"] == B::FAMILY && c["topic"] == B::TOPIC && c["direction"] == direction
        }));
    }

    fn manual_command(linear_x_mps: f64, angular_z_radps: f64) -> api::motion::ManualCommand {
        api::motion::ManualCommand {
            linear_x_mps,
            angular_z_radps,
        }
    }

    fn follow_target(linear_x_mps: f64, angular_z_radps: f64) -> api::follow::Target {
        api::follow::Target {
            map_revision: Some(1),
            built_from_localize_revision: None,
            frame_id: "map".to_string(),
            linear_x_mps,
            angular_z_radps,
        }
    }

    fn safety_authorization(
        decision: api::safety::SafetyDecision,
    ) -> api::safety::SafetyAuthorization {
        safety_authorization_with_limits(decision, 1.0, 1.0)
    }

    fn safety_authorization_with_expiry(
        decision: api::safety::SafetyDecision,
        expires_at_ns: Option<u64>,
    ) -> api::safety::SafetyAuthorization {
        let mut authorization = safety_authorization_with_limits(decision, 1.0, 1.0);
        authorization.expires_at_ns = expires_at_ns;
        authorization
    }

    fn safety_authorization_with_escape(
        decision: api::safety::SafetyDecision,
        reverse_mps: f64,
        angular_radps: f64,
    ) -> api::safety::SafetyAuthorization {
        api::safety::SafetyAuthorization {
            decision,
            approved_motion: api::safety::MotionConstraint {
                linear_x_mps: api::safety::Constraint {
                    min: -reverse_mps,
                    max: 0.0,
                },
                angular_z_radps: api::safety::Constraint {
                    min: -angular_radps,
                    max: angular_radps,
                },
            },
            reasons: Vec::new(),
            source_revision: api::safety::SafetySourceRevision {
                localization: None,
                map: None,
            },
            expires_at_ns: Some(NOW_NS + 1_000_000_000),
        }
    }

    fn safety_authorization_with_limits(
        decision: api::safety::SafetyDecision,
        linear_x_mps: f64,
        angular_radps: f64,
    ) -> api::safety::SafetyAuthorization {
        api::safety::SafetyAuthorization {
            decision,
            approved_motion: api::safety::MotionConstraint {
                linear_x_mps: api::safety::Constraint {
                    min: -linear_x_mps,
                    max: linear_x_mps,
                },
                angular_z_radps: api::safety::Constraint {
                    min: -angular_radps,
                    max: angular_radps,
                },
            },
            reasons: Vec::new(),
            source_revision: api::safety::SafetySourceRevision {
                localization: None,
                map: None,
            },
            expires_at_ns: Some(NOW_NS + 1_000_000_000),
        }
    }

    fn malformed_safety_authorization() -> api::safety::SafetyAuthorization {
        api::safety::SafetyAuthorization {
            decision: api::safety::SafetyDecision::Allow,
            approved_motion: api::safety::MotionConstraint {
                linear_x_mps: api::safety::Constraint {
                    min: 1.0,
                    max: -1.0,
                },
                angular_z_radps: api::safety::Constraint {
                    min: 0.5,
                    max: -0.5,
                },
            },
            reasons: Vec::new(),
            source_revision: api::safety::SafetySourceRevision {
                localization: None,
                map: None,
            },
            expires_at_ns: Some(NOW_NS + 1_000_000_000),
        }
    }

    fn timed<T>(produced_at_ns: u64, body: T) -> Timed<T> {
        Timed {
            body,
            produced_at_ns,
        }
    }
}
