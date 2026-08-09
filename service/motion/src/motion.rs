//! `motion` - the sole body-twist arbiter and emergency-stop authority.
//!
//! Manual and autonomous candidates are freshness checked, finite-value checked,
//! and clamped to the robot-authored motion limits. Motion aggregates every
//! per-component emergency-stop state before publishing `drive/target`, and the
//! safety service contributes typed world-aware constraints. Autonomous
//! candidates require fresh constraints; direct manual control may recover
//! without that provider, while still obeying e-stop, the manual lease,
//! finite-value, and robot-limit gates.
//!
//! # Emergency stop has no privileged path
//!
//! Every emergency stop is a manifest-declared `emergency_stop` component that
//! publishes state like any other component: zero declared components means
//! zero subscriptions (so a robot without e-stop hardware still supports direct
//! teleoperation), and a configured-but-silent publisher fails closed exactly
//! like any other configured input. The only thing specific to e-stop is *value
//! aggregation* - an engage observed during a cycle wins even if a release
//! follows in the same cycle - which is ordinary domain logic, not
//! communication semantics.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use anyhow::{Result, bail};
use phoxal::api;
use phoxal::model::component::capability::Capability;
use phoxal::model::identity::CapabilityRef;
use phoxal::model::robot::MotionLimits;
use phoxal::prelude::*;

use crate::arbitration::{
    AUTONOMOUS_HOLD, AUTONOMOUS_SILENCE, Arbitration, MANUAL_HOLD, MANUAL_SILENCE,
    candidate_age_ns, manual_observed_age_ns, safety_is_usable,
};

/// A configured component emergency stop must keep publishing. Silence past
/// this window fails closed, exactly like any other configured input.
const COMPONENT_ESTOP_STALE: Duration = Duration::from_secs(1);

/// One declared emergency stop and the subscription carrying its state.
struct BoundEmergencyStop {
    reference: CapabilityRef,
    state: StateView<api::endpoint::component::emergency_stop::StateEndpoint>,
}

/// Aggregated emergency-stop state across every declared component.
///
/// Zero declared components is a valid robot: the latch is then trivially
/// clear and direct teleoperation works. Each declared component is required,
/// so a missing or stale sample blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
struct EmergencyStopLatch {
    /// The newest engaged/released report per declared e-stop. Keying on the
    /// capability rather than on a subscriber position means a report can never
    /// be attributed to a different e-stop than the one that published it.
    component_state: BTreeMap<CapabilityRef, Option<Timed<bool>>>,
    engage_seen_this_cycle: bool,
}

impl EmergencyStopLatch {
    fn new(references: impl IntoIterator<Item = CapabilityRef>) -> Self {
        Self {
            component_state: references
                .into_iter()
                .map(|reference| (reference, None))
                .collect(),
            engage_seen_this_cycle: false,
        }
    }

    fn set_component(&mut self, reference: &CapabilityRef, engaged: bool, at: RobotInstant) {
        self.engage_seen_this_cycle |= engaged;
        if let Some(current) = self.component_state.get_mut(reference) {
            *current = Some(Timed::new(engaged, at));
        }
    }

    /// Whether any declared component blocks motion: engaged, never heard from,
    /// or silent past the staleness window.
    fn components_blocked(&self, now: RobotInstant) -> bool {
        self.component_state.values().any(|sample| {
            let Some(sample) = sample else {
                return true;
            };
            sample.body || !sample.fresh_within(now, COMPONENT_ESTOP_STALE)
        })
    }

    fn engaged(&self, now: RobotInstant) -> bool {
        self.engage_seen_this_cycle || self.components_blocked(now)
    }

    fn finish_cycle(&mut self) {
        self.engage_seen_this_cycle = false;
    }

    fn reset_timeline(&mut self) {
        // Component samples describe the replaced simulated world.
        for sample in self.component_state.values_mut() {
            *sample = None;
        }
    }
}

pub(crate) struct Api {
    manual: SetpointReceiver<api::endpoint::motion::ManualEndpoint>,
    autonomous: StateView<api::endpoint::navigation::CandidateEndpoint>,
    _navigation_ready: phoxal::bus::ParticipantReadyObserver,
    _safety_ready: phoxal::bus::ParticipantReadyObserver,
    component_estops: Vec<BoundEmergencyStop>,
    safety_constraints: StateView<api::endpoint::safety::ConstraintsEndpoint>,
    drive: SetpointPublisher<api::endpoint::drive::TargetEndpoint>,
    state: StatePublisher<api::endpoint::motion::StateEndpoint>,
}

pub(crate) struct MotionState {
    limits: MotionLimits,
    manual: ExclusiveProducerLease<api::motion::ManualCommand>,
    manual_observed_at: Option<LocalInstant>,
    last_autonomous: Option<Timed<api::navigation::Candidate>>,
    last_autonomous_offer: Option<NavigationOfferKey>,
    autonomous_admission: Arc<Mutex<FixedSourceAdmission>>,
    autonomous_authority: Arc<Mutex<FixedSourceLease<api::navigation::Candidate>>>,
    estop: EmergencyStopLatch,
    last_safety_constraints: Option<Timed<api::safety::MotionConstraints>>,
}

#[phoxal::service(state = MotionState, api = Api)]
pub(crate) struct Motion;

type NavigationOfferKey = (phoxal::bus::ProducerId, u64, u64);

fn current_navigation_offer_key(
    admission: &FixedSourceAdmission,
    source: Option<&phoxal::bus::ParticipantSourceIdentity>,
    sequence: u64,
) -> Option<NavigationOfferKey> {
    if !admission.is_current(source, sequence) {
        return None;
    }
    Some((source?.producer, sequence, admission.ready_generation()))
}

impl Participant for Motion {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let robot = ctx.robot()?;
        let navigation = phoxal::bus::ParticipantId::new("navigation")
            .map_err(|error| anyhow::anyhow!("invalid fixed navigation participant id: {error}"))?;
        let navigation_authority = Arc::new(Mutex::new(FixedSourceLease::new(
            "navigation/candidate",
            navigation.clone(),
            AUTONOMOUS_SILENCE,
            AUTONOMOUS_HOLD,
        )));
        let navigation_admission =
            Arc::new(Mutex::new(FixedSourceAdmission::new(navigation.clone())));
        let ready_authority = Arc::clone(&navigation_authority);
        let ready_admission = Arc::clone(&navigation_admission);
        let navigation_ready = ctx
            .observe_participant_ready_for(&navigation, move |event| {
                if let Ok(mut authority) = ready_authority.lock() {
                    authority.update_ready_event(&event);
                }
                if let Ok(mut admission) = ready_admission.lock() {
                    admission.update_ready_event(&event);
                }
            })
            .await?;
        let candidate_admission = Arc::clone(&navigation_admission);
        let safety = phoxal::bus::ParticipantId::new("safety")
            .map_err(|error| anyhow::anyhow!("invalid fixed safety participant id: {error}"))?;
        let limits = robot.motion().limits().validate()?;
        let estops =
            robot.capability_refs(|capability| matches!(capability, Capability::EmergencyStop(_)));

        let mut component_estops = Vec::with_capacity(estops.len());
        for reference in &estops {
            component_estops.push(BoundEmergencyStop {
                reference: reference.clone(),
                state: ctx
                    .state_view(
                        api::topic::client()
                            .component(&reference.component_id)?
                            .emergency_stop(&reference.capability_id)?
                            .state(),
                    )
                    .await?,
            });
        }

        let safety_authority = Arc::new(Mutex::new(FixedSourceAdmission::new(safety.clone())));
        let ready_authority = Arc::clone(&safety_authority);
        let safety_ready = ctx
            .observe_participant_ready_for(&safety, move |event| {
                if let Ok(mut authority) = ready_authority.lock() {
                    authority.update_ready_event(&event);
                }
            })
            .await?;
        let admission_authority = Arc::clone(&safety_authority);

        Ok((
            MotionState {
                limits,
                manual: ExclusiveProducerLease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD),
                manual_observed_at: None,
                last_autonomous: None,
                last_autonomous_offer: None,
                autonomous_admission: navigation_admission,
                autonomous_authority: navigation_authority,
                estop: EmergencyStopLatch::new(estops),
                last_safety_constraints: None,
            },
            Api {
                manual: ctx
                    .setpoint_receiver(api::topic::owner().motion().manual())
                    .await?,
                autonomous: ctx
                    .state_view_with_admission(
                        api::topic::client().navigation().candidate(),
                        move |observed| {
                            let Ok(mut admission) = candidate_admission.lock() else {
                                return false;
                            };
                            matches!(
                                admission.offer(
                                    observed.metadata.source.participant_source(),
                                    observed.metadata.sequence,
                                ),
                                LeaseDecision::Acquired | LeaseDecision::Renewed
                            )
                        },
                    )
                    .await?,
                _navigation_ready: navigation_ready,
                _safety_ready: safety_ready,
                component_estops,
                safety_constraints: ctx
                    .state_view_with_admission(
                        api::topic::client().safety().constraints(),
                        move |observed| {
                            let Ok(mut authority) = admission_authority.lock() else {
                                return false;
                            };
                            matches!(
                                authority.offer(
                                    observed.metadata.source.participant_source(),
                                    observed.metadata.sequence,
                                ),
                                LeaseDecision::Acquired | LeaseDecision::Renewed
                            )
                        },
                    )
                    .await?,
                drive: ctx.setpoint_publisher(api::topic::client().drive().target())?,
                state: ctx.state_publisher(api::topic::owner().motion().state())?,
            },
        ))
    }

    fn reset(&self, _ctx: ResetContext, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        state.last_autonomous = None;
        state.last_autonomous_offer = None;
        let Ok(mut authority) = state.autonomous_authority.lock() else {
            return Err(anyhow::anyhow!("navigation authority lock was poisoned"));
        };
        authority.clear();
        state.last_safety_constraints = None;
        state.estop.reset_timeline();
        // The manual command is a clockless operator input sampled at a logical
        // step, not state derived from the replaced world, so the lease is not
        // cleared here and keeps ageing on the host clock across the boundary.
        // What happens to a held command depends on whether it had already been
        // applied: one anchored on the retired timeline is dropped at the first
        // step of the replacement, because its horizon cannot be measured
        // across worlds; one that arrived but has not yet been applied anchors
        // on that first step instead. Either way the machine keeps moving only
        // while the operator keeps publishing.
        Ok(())
    }

    #[phoxal::step(hz = 20)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        let now = step.now();
        // Without the host clock there is no silence deadline to measure, so
        // this step decides nothing: it renews no lease and applies no
        // command, and the leases expire on their own. The runner's own clock
        // read faults the participant on the same step.
        let Some(host_now) = LocalInstant::try_now() else {
            bail!("the host boot clock could not be read");
        };

        while let Some(observed) = api.manual.try_recv() {
            // The receiver may hold one pending intent per producer. Expire
            // the current owner before every offer so a stale first item
            // cannot reacquire the lease and block a fresh later producer in
            // the same step.
            state.manual.expire_before_offer(host_now, now);
            let observed_at = observed.observed_at;
            match state.manual.offer(
                &observed.metadata.source,
                observed.metadata.sequence,
                observed_at,
                observed.body,
            ) {
                LeaseDecision::Rejected(rejection) => {
                    tracing::warn!(
                        target: "phoxal.motion",
                        error = %rejection,
                        "rejected manual command"
                    );
                }
                _ => state.manual_observed_at = Some(observed_at),
            }
        }
        if let Some(observed) = api.autonomous.observed()
            && let Some(at) = observed.metadata.produced_exactly_at()
        {
            let source = observed.metadata.source.participant_source();
            let offer_key = {
                let Ok(admission) = state.autonomous_admission.lock() else {
                    bail!("navigation admission lock was poisoned");
                };
                current_navigation_offer_key(&admission, source, observed.metadata.sequence)
            };
            let mut accepted_for_liveness =
                offer_key.is_some() && state.last_autonomous_offer == offer_key;
            if let Some(offer_key) = offer_key
                && state.last_autonomous_offer != Some(offer_key)
            {
                let Ok(mut authority) = state.autonomous_authority.lock() else {
                    bail!("navigation authority lock was poisoned");
                };
                let decision = authority.offer(
                    source,
                    observed.metadata.sequence,
                    observed.observed_at,
                    observed.body.clone(),
                );
                state.last_autonomous_offer = Some(offer_key);
                accepted_for_liveness = !matches!(decision, LeaseDecision::Rejected(_));
                if let LeaseDecision::Rejected(rejection) = decision {
                    tracing::warn!(
                        target: "phoxal.motion",
                        error = %rejection,
                        "rejected visible navigation candidate"
                    );
                }
            }
            if accepted_for_liveness {
                state.last_autonomous = Some(Timed::new(observed.body.clone(), at));
            }
        }
        let navigation_live = {
            let Ok(mut authority) = state.autonomous_authority.lock() else {
                bail!("navigation authority lock was poisoned");
            };
            authority.live(host_now, now).is_some()
        };
        if !navigation_live {
            state.last_autonomous = None;
        }
        for bound in &api.component_estops {
            if let Some(observed) = bound.state.observed()
                && let Some(at) = observed.metadata.produced_exactly_at()
            {
                state
                    .estop
                    .set_component(&bound.reference, observed.body.engaged, at);
            }
        }
        if let Some(observed) = api.safety_constraints.observed()
            && let Some(at) = observed.metadata.produced_exactly_at()
        {
            state.last_safety_constraints = Some(Timed::new(observed.body.clone(), at));
        }

        let safety_runtime = if state
            .last_safety_constraints
            .as_ref()
            .is_some_and(|constraints| safety_is_usable(constraints, now))
        {
            api::motion::SafetyRuntime::Present
        } else {
            api::motion::SafetyRuntime::Absent
        };
        let manual = state.manual.live(host_now, now).cloned();
        if manual.is_none() {
            state.manual_observed_at = None;
        }
        let arbitration = Arbitration::decide(
            manual.as_ref(),
            state.last_autonomous.as_ref(),
            state.estop.engaged(now),
            state.last_safety_constraints.as_ref(),
            state.limits,
            now,
        );
        state.estop.finish_cycle();

        let drive_target = match &arbitration.decision {
            api::motion::Decision::Active { target, .. } => target.clone(),
            api::motion::Decision::Stopped { .. } => api::drive::Target::stopped(),
        };
        api.drive.send(drive_target)?;
        api.state.publish(
            &step.token,
            api::motion::State {
                decision: arbitration.decision,
                manual_observed_age_ns: manual_observed_age_ns(state.manual_observed_at, host_now),
                autonomous_candidate_age_ns: candidate_age_ns(state.last_autonomous.as_ref(), now),
                safety_constraints_age_ns: candidate_age_ns(
                    state.last_safety_constraints.as_ref(),
                    now,
                ),
                safety_runtime,
                component_estop_blocked: state.estop.components_blocked(now),
                active_safety_constraints: state.last_safety_constraints.as_ref().map_or_else(
                    Vec::new,
                    |constraints| {
                        if safety_is_usable(constraints, now) {
                            constraints.body.constraints.clone()
                        } else {
                            Vec::new()
                        }
                    },
                ),
                safety_permission: state
                    .last_safety_constraints
                    .as_ref()
                    .filter(|constraints| safety_is_usable(constraints, now))
                    .map_or(
                        api::safety::MotionPermission::Stopped {
                            reasons: vec![api::safety::ConstraintReason::MapUnavailable],
                        },
                        |constraints| constraints.body.permission.clone(),
                    ),
            },
        )?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use phoxal::bus::{
        FixedSourceAdmission, FixedSourceLease, LeaseDecision, LeaseRejection, ParticipantId,
        ParticipantReadyStatus, ParticipantSourceIdentity, ProducerId, SourceAttribution,
        TimelineId,
    };

    use super::*;

    /// A distinct deterministic test producer. Production sessions mint their
    /// producer through the bus owner, while tests name theirs explicitly.
    fn producer(value: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | value).expect("a test producer is canonical")
    }

    fn external(producer: ProducerId) -> SourceAttribution {
        SourceAttribution::External {
            producer,
            label: None,
        }
    }

    fn line() -> TimelineId {
        TimelineId::from_raw(1).expect("test timeline must be nonzero")
    }

    fn navigation_source(producer: ProducerId) -> ParticipantSourceIdentity {
        ParticipantSourceIdentity::new(
            ParticipantId::new("navigation").expect("valid navigation participant"),
            producer,
        )
    }

    fn at(ticks: u64) -> RobotInstant {
        RobotInstant::new(line(), ticks)
    }

    #[test]
    fn navigation_ready_loss_requires_fresh_same_sequence_for_liveness() {
        let source = navigation_source(producer(22));
        let observed_at = LocalInstant::from_boot_ns(0);
        let mut admission = FixedSourceAdmission::new(source.participant.clone());
        let mut authority = FixedSourceLease::new(
            "navigation/candidate",
            source.participant.clone(),
            Duration::from_secs(1),
            Duration::from_secs(1),
        );
        admission.update_ready(&source, ParticipantReadyStatus::Ready);
        authority.update_ready(&source, ParticipantReadyStatus::Ready);

        assert_eq!(admission.offer(Some(&source), 7), LeaseDecision::Acquired);
        let mut last_offer = Some(
            current_navigation_offer_key(&admission, Some(&source), 7)
                .expect("the first candidate is current"),
        );
        assert_eq!(
            authority.offer(Some(&source), 7, observed_at, "T1"),
            LeaseDecision::Acquired
        );

        // Ready loss invalidates the retained candidate immediately. The
        // step-facing StateView may still expose T1, but it must not reacquire
        // the lease after Ready returns.
        admission.update_ready(&source, ParticipantReadyStatus::Lost);
        authority.update_ready(&source, ParticipantReadyStatus::Lost);
        admission.update_ready(&source, ParticipantReadyStatus::Ready);
        authority.update_ready(&source, ParticipantReadyStatus::Ready);
        assert!(current_navigation_offer_key(&admission, Some(&source), 7).is_none());
        assert_eq!(
            authority.live(observed_at, at(0)),
            None,
            "retained T1 must not become live again"
        );
        assert!(
            last_offer.is_some(),
            "the old marker remains historical evidence"
        );

        // A fresh ingress publication with the same producer and sequence has
        // a new Ready generation and therefore receives a new offer key.
        assert_eq!(admission.offer(Some(&source), 7), LeaseDecision::Acquired);
        let fresh_offer = current_navigation_offer_key(&admission, Some(&source), 7)
            .expect("fresh ingress is current");
        assert_ne!(last_offer, Some(fresh_offer));
        assert_eq!(
            authority.offer(Some(&source), 7, observed_at, "fresh"),
            LeaseDecision::Acquired
        );
        last_offer = Some(fresh_offer);
        assert_eq!(authority.live(observed_at, at(0)), Some(&"fresh"));
        assert_eq!(last_offer, Some(fresh_offer));
    }

    fn estop(index: u8) -> CapabilityRef {
        format!("estop{index}.stop")
            .parse()
            .expect("a test e-stop reference is well formed")
    }

    fn latch(count: u8) -> EmergencyStopLatch {
        EmergencyStopLatch::new((0..count).map(estop))
    }

    /// The manual command runs on the same receiver-owned lease as any other
    /// leased input, so it needs no bespoke path: silence on the host clock and
    /// travel on robot time each expire it independently.
    #[test]
    fn the_manual_lease_expires_on_host_silence_and_on_the_logical_horizon() {
        let command = api::motion::ManualCommand {
            linear_x_mps: 0.4,
            angular_z_radps: 0.0,
        };
        let producer = producer(1);
        let host_start = LocalInstant::from_boot_ns(0);

        let mut silent = ExclusiveProducerLease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD);
        silent.offer(&external(producer), 1, host_start, command.clone());
        assert!(silent.live(host_start, at(0)).is_some());
        assert!(
            silent
                .live(
                    host_start.saturating_add(MANUAL_SILENCE + Duration::from_millis(1)),
                    at(0)
                )
                .is_none()
        );

        let mut held = ExclusiveProducerLease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD);
        held.offer(&external(producer), 1, host_start, command);
        assert!(held.live(host_start, at(0)).is_some());
        let past_hold = u64::try_from(MANUAL_HOLD.as_nanos()).unwrap() + 1;
        assert!(held.live(host_start, at(past_hold)).is_none());
    }

    #[test]
    fn stale_manual_a_is_expired_before_fresh_b_first_offer() {
        let first = producer(3);
        let second = producer(4);
        let host_start = LocalInstant::from_boot_ns(0);
        let fresh_at = host_start.saturating_add(MANUAL_SILENCE + Duration::from_millis(1));
        let mut lease = ExclusiveProducerLease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD);

        // The first pending value is stale by the time this step drains it.
        // Both checks use the step's current host time, as Motion does.
        lease.expire_before_offer(fresh_at, at(0));
        assert_eq!(
            lease.offer(&external(first), 1, host_start, 1_u8),
            LeaseDecision::Acquired
        );

        // This is the same order as Motion's pending-receiver drain: the
        // owner expiry check is immediately before each offer. Without the
        // second check, stale A would still own the lease and fresh B's first
        // command would be rejected.
        lease.expire_before_offer(fresh_at, at(0));
        assert_eq!(
            lease.offer(&external(second), 1, fresh_at, 3_u8),
            LeaseDecision::Acquired
        );
        assert_eq!(lease.producer(), Some(second));
    }

    #[test]
    fn continuous_manual_a_beats_faster_b_past_manual_silence() {
        let first = producer(5);
        let second = producer(6);
        let host_start = LocalInstant::from_boot_ns(0);
        let mut lease = ExclusiveProducerLease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD);
        let mut first_sequence = 0;
        let mut second_sequence = 0;

        // B floods every 10 ms while A renews every 50 ms. The exchange lasts
        // well beyond MANUAL_SILENCE, but A's continuous publishing keeps its
        // receiver-owned authority and every faster B offer is rejected.
        for tick_ms in 0..=500 {
            let now = host_start.saturating_add(Duration::from_millis(tick_ms));
            if tick_ms % 50 == 0 {
                first_sequence += 1;
                lease.expire_before_offer(now, at(0));
                assert!(matches!(
                    lease.offer(&external(first), first_sequence, now, 1_u8),
                    LeaseDecision::Acquired | LeaseDecision::Renewed
                ));
            }
            if tick_ms % 10 == 0 {
                second_sequence += 1;
                lease.expire_before_offer(now, at(0));
                assert!(matches!(
                    lease.offer(&external(second), second_sequence, now, 2_u8),
                    LeaseDecision::Rejected(LeaseRejection::AuthorityHeld { owner })
                        if owner == first
                ));
            }
        }

        assert!(Duration::from_millis(500) > MANUAL_SILENCE);
        assert_eq!(lease.producer(), Some(first));
        assert_eq!(
            lease.live(host_start.saturating_add(Duration::from_millis(500)), at(0)),
            Some(&1)
        );
    }

    /// A lease that expired while the simulation was paused must not apply on
    /// the first resumed arbitration step.
    #[test]
    fn a_lease_expired_while_paused_does_not_apply_on_the_first_resumed_step() {
        let producer = producer(2);
        let host_start = LocalInstant::from_boot_ns(0);
        let mut lease = ExclusiveProducerLease::new("motion/manual", MANUAL_SILENCE, MANUAL_HOLD);
        lease.offer(
            &external(producer),
            1,
            host_start,
            api::motion::ManualCommand {
                linear_x_mps: 0.4,
                angular_z_radps: 0.0,
            },
        );
        assert!(lease.live(host_start, at(0)).is_some());

        // Steps stop while host time keeps running past the silence deadline.
        let resumed = host_start.saturating_add(MANUAL_SILENCE + Duration::from_millis(1));
        assert!(lease.live(resumed, at(1)).is_none());
    }

    #[test]
    fn component_estops_latch_independently_and_release_together() {
        let mut latch = latch(2);
        let now = at(1_000);
        assert!(
            latch.engaged(now),
            "configured component e-stops must publish before motion"
        );
        latch.set_component(&estop(0), false, now);
        latch.set_component(&estop(1), false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));

        latch.set_component(&estop(1), true, now);
        latch.finish_cycle();
        assert!(latch.engaged(now));
        latch.set_component(&estop(1), false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));
    }

    /// Value aggregation: an engage observed during a cycle wins even if a
    /// release follows in the same cycle. This is domain logic, not a
    /// privileged communication path, so it is exercised through an ordinary
    /// component sample.
    #[test]
    fn engage_then_release_in_one_cycle_still_forces_a_stop_cycle() {
        let mut latch = latch(1);
        let now = at(1_000);
        latch.set_component(&estop(0), true, now);
        latch.set_component(&estop(0), false, now);
        assert!(latch.engaged(now));
        latch.finish_cycle();
        assert!(!latch.engaged(now));
    }

    #[test]
    fn a_robot_without_component_estops_starts_ready_for_manual_control() {
        let latch = latch(0);
        assert!(
            !latch.engaged(at(1_000)),
            "zero declared components means zero subscriptions, so direct teleoperation is valid"
        );
    }

    #[test]
    fn missing_stale_future_and_replaced_timeline_component_estops_fail_closed() {
        let stale_ns = u64::try_from(COMPONENT_ESTOP_STALE.as_nanos()).unwrap();
        let now = at(stale_ns + 10);
        let mut latch = latch(1);
        assert!(
            latch.components_blocked(now),
            "a missing sample fails closed"
        );

        latch.set_component(&estop(0), false, at(9));
        assert!(latch.components_blocked(now), "a stale sample fails closed");
        latch.set_component(&estop(0), false, at(now.ticks() + 1));
        assert!(
            latch.components_blocked(now),
            "a sample from this step's future fails closed"
        );
        latch.set_component(
            &estop(0),
            false,
            RobotInstant::new(TimelineId::mint(), now.ticks()),
        );
        assert!(
            latch.components_blocked(now),
            "a sample from a replaced world is incomparable, so it fails closed"
        );
        latch.set_component(&estop(0), false, now);
        latch.finish_cycle();
        assert!(!latch.components_blocked(now));
    }

    #[test]
    fn a_replaced_timeline_clears_component_samples_so_they_must_republish() {
        let mut latch = latch(1);
        let now = at(1_000);
        latch.set_component(&estop(0), false, now);
        latch.finish_cycle();
        assert!(!latch.engaged(now));

        latch.reset_timeline();
        assert!(
            latch.engaged(now),
            "after a world replacement every configured component must publish again"
        );
    }
}
