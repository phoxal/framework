//! `safety` - world-input assessment into typed, expiring motion constraints.
//!
//! Emergency stop remains owned by `motion`; safety consumes localization, map,
//! proximity, drive, and optional battery state and emits one diagnosable
//! constraint product. Autonomous motion treats a missing, stale, future-dated,
//! or retired-timeline product as a stop. Manual recovery can operate without that
//! product, while any valid protective stop or limit still applies.

#[cfg(test)]
use anyhow::Context;
use anyhow::{Result, bail};
use std::collections::BTreeMap;
use std::time::Duration;

use phoxal::api;
use phoxal::bus::ContractBody;
use phoxal::model::component::capability::Capability;
use phoxal::model::identity::CapabilityRef;
use phoxal::prelude::*;

const INPUT_STALE: Duration = Duration::from_nanos(1_000_000_000);
const MAP_STALE: Duration = Duration::from_nanos(600_000_000);
const CONSTRAINT_TTL: Duration = Duration::from_millis(300);
const MIN_LOCALIZATION_CONFIDENCE: f32 = 0.25;
const PROTECTIVE_STOP_DISTANCE_M: f32 = 0.25;
const PROXIMITY_LIMIT_DISTANCE_M: f32 = 0.60;
const PROXIMITY_LINEAR_LIMIT_MPS: f32 = 0.15;

// The participant a constraint names as its source. These are published wire
// values, so each spelling is written once here rather than at the call sites
// that build a constraint.
const LOCALIZE_PARTICIPANT_ID: &str = "localize";
const MAP_PARTICIPANT_ID: &str = "map";
const DRIVE_PARTICIPANT_ID: &str = "drive";
const BATTERY_PARTICIPANT_ID: &str = "battery-provider";
const WORLD_MODEL_PARTICIPANT_ID: &str = "safety";
const OPERATOR_PARTICIPANT_ID: &str = "operator";
/// A range constraint normally names the component that observed it. This is
/// the fallback for a range source raised with no bound capability behind it.
const RANGE_PARTICIPANT_ID: &str = "range-provider";

/// One declared component capability and the subscription carrying its samples.
struct BoundInput<B> {
    reference: CapabilityRef,
    samples: Subscriber<B>,
}

/// The newest sample of every world input this service reads.
///
/// The per-capability inputs are keyed by the capability they came from, not by
/// a subscriber position, so a sample can never be attributed to a different
/// sensor than the one that published it. A declared capability with no sample
/// yet is present with `None`, which is what lets a silent range sensor raise a
/// constraint naming itself.
struct WorldInputs {
    localization: Option<Timed<api::localize::LocalizationState>>,
    map: Option<Timed<api::map::Revision>>,
    drivable_space: Option<Timed<bool>>,
    drive: Option<Timed<api::drive::State>>,
    batteries: BTreeMap<CapabilityRef, Option<Timed<api::component::battery::State>>>,
    ranges: BTreeMap<CapabilityRef, Option<Timed<api::component::range::Sample>>>,
}

impl WorldInputs {
    fn new(
        ranges: impl IntoIterator<Item = CapabilityRef>,
        batteries: impl IntoIterator<Item = CapabilityRef>,
    ) -> Self {
        Self {
            localization: None,
            map: None,
            drivable_space: None,
            drive: None,
            batteries: batteries
                .into_iter()
                .map(|reference| (reference, None))
                .collect(),
            ranges: ranges
                .into_iter()
                .map(|reference| (reference, None))
                .collect(),
        }
    }

    /// Drop every sample while keeping the declared capabilities: the samples
    /// describe a world that has been replaced, the declarations have not.
    fn clear(&mut self) {
        self.localization = None;
        self.map = None;
        self.drivable_space = None;
        self.drive = None;
        for sample in self.batteries.values_mut() {
            *sample = None;
        }
        for sample in self.ranges.values_mut() {
            *sample = None;
        }
    }

    /// Assess this step's world into one constraint product.
    ///
    /// Range constraints are emitted in declared `(component id, capability id)`
    /// order, because [`Robot::capability_refs`](phoxal::model::Robot::capability_refs)
    /// guarantees that order and the map preserves it: two runs over the same
    /// robot must produce the same constraint sequence, and the nearest-obstacle
    /// tie-break must resolve the same way.
    fn assess(&self, sequence: u64, now: RobotInstant) -> Result<api::safety::MotionConstraints> {
        let expires_at = now.saturating_add(CONSTRAINT_TTL);
        let mut constraints = Vec::new();

        match usable(self.localization.as_ref(), now, INPUT_STALE) {
            None => constraints.push(stop_constraint(
                api::safety::ConstraintReason::LocalizationUnavailable,
                source(api::safety::ConstraintSourceKind::Localization, None),
                None,
                now,
                expires_at,
            )),
            Some(localization) => {
                if !(localization.x_m.is_finite()
                    && localization.y_m.is_finite()
                    && localization.yaw_rad.is_finite()
                    && localization.confidence.is_finite())
                {
                    bail!("localization world input contains a non-finite value");
                }
                if localization.confidence < MIN_LOCALIZATION_CONFIDENCE {
                    constraints.push(stop_constraint(
                        api::safety::ConstraintReason::LocalizationUncertain,
                        source(api::safety::ConstraintSourceKind::Localization, None),
                        Some(localization.confidence),
                        now,
                        expires_at,
                    ));
                }
            }
        }

        if usable(self.map.as_ref(), now, MAP_STALE).is_none() {
            constraints.push(stop_constraint(
                api::safety::ConstraintReason::MapUnavailable,
                source(api::safety::ConstraintSourceKind::Map, None),
                None,
                now,
                expires_at,
            ));
        }

        match usable(self.drivable_space.as_ref(), now, MAP_STALE) {
            None => constraints.push(stop_constraint(
                api::safety::ConstraintReason::WorldUnavailable,
                source(api::safety::ConstraintSourceKind::WorldModel, None),
                None,
                now,
                expires_at,
            )),
            Some(false) => constraints.push(stop_constraint(
                api::safety::ConstraintReason::DrivableSpaceUnavailable,
                source(api::safety::ConstraintSourceKind::WorldModel, None),
                None,
                now,
                expires_at,
            )),
            Some(true) => {}
        }

        let mut nearest_range = None::<(f32, &CapabilityRef)>;
        for (reference, sample) in &self.ranges {
            let Some(sample) = usable(sample.as_ref(), now, INPUT_STALE) else {
                constraints.push(stop_constraint(
                    api::safety::ConstraintReason::WorldUnavailable,
                    source(api::safety::ConstraintSourceKind::Range, Some(reference)),
                    None,
                    now,
                    expires_at,
                ));
                continue;
            };
            if !sample.distance_m.is_finite()
                || matches!(sample.health, api::component::range::SensorHealth::Fault)
                || sample
                    .quality
                    .as_ref()
                    .is_some_and(|quality| !quality.valid)
            {
                constraints.push(stop_constraint(
                    api::safety::ConstraintReason::RangeSensorFault,
                    source(api::safety::ConstraintSourceKind::Range, Some(reference)),
                    Some(sample.distance_m),
                    now,
                    expires_at,
                ));
                continue;
            }
            if nearest_range.is_none_or(|(distance, _)| sample.distance_m < distance) {
                nearest_range = Some((sample.distance_m, reference));
            }
        }

        if let Some((distance, reference)) = nearest_range {
            if distance <= PROTECTIVE_STOP_DISTANCE_M {
                constraints.push(stop_constraint(
                    api::safety::ConstraintReason::ObstacleProximity,
                    source(api::safety::ConstraintSourceKind::Range, Some(reference)),
                    Some(distance),
                    now,
                    expires_at,
                ));
            } else if distance <= PROXIMITY_LIMIT_DISTANCE_M {
                constraints.push(limit_constraint(
                    api::safety::ConstraintReason::ObstacleProximity,
                    source(api::safety::ConstraintSourceKind::Range, Some(reference)),
                    PROXIMITY_LINEAR_LIMIT_MPS,
                    Some(distance),
                    now,
                    expires_at,
                ));
            }
        }

        if let Some(drive) = usable(self.drive.as_ref(), now, INPUT_STALE)
            && matches!(
                drive,
                api::drive::State::Stopped {
                    reason: api::drive::StopReason::Fault
                        | api::drive::StopReason::ActuatorCommandNotFinite,
                    ..
                }
            )
        {
            constraints.push(stop_constraint(
                api::safety::ConstraintReason::DriveFault,
                source(api::safety::ConstraintSourceKind::Drive, None),
                None,
                now,
                expires_at,
            ));
        }

        // A robot may carry several packs. The emptiest usable one decides: a
        // healthy pack cannot vouch for a flat one.
        let mut lowest_charge_ratio: Option<f32> = None;
        for battery in self.batteries.values() {
            let Some(battery) = usable(battery.as_ref(), now, INPUT_STALE) else {
                continue;
            };
            if !battery.charge_ratio.is_finite() {
                bail!("battery world input contains a non-finite charge ratio");
            }
            lowest_charge_ratio = Some(match lowest_charge_ratio {
                Some(lowest) => lowest.min(battery.charge_ratio),
                None => battery.charge_ratio,
            });
        }
        if let Some(charge_ratio) = lowest_charge_ratio {
            if charge_ratio <= 0.05 {
                constraints.push(stop_constraint(
                    api::safety::ConstraintReason::BatteryCritical,
                    source(api::safety::ConstraintSourceKind::Battery, None),
                    Some(charge_ratio),
                    now,
                    expires_at,
                ));
            } else if charge_ratio <= 0.15 {
                constraints.push(limit_constraint(
                    api::safety::ConstraintReason::BatteryLow,
                    source(api::safety::ConstraintSourceKind::Battery, None),
                    PROXIMITY_LINEAR_LIMIT_MPS,
                    Some(charge_ratio),
                    now,
                    expires_at,
                ));
            }
        }

        let stop = constraints.iter().any(|constraint| constraint.stop);
        let max_linear_speed_mps = constraints
            .iter()
            .filter_map(|constraint| constraint.max_linear_speed_mps)
            .reduce(f32::min);
        let max_angular_speed_radps = constraints
            .iter()
            .filter_map(|constraint| constraint.max_angular_speed_radps)
            .reduce(f32::min);
        Ok(api::safety::MotionConstraints {
            sequence,
            stop,
            max_linear_speed_mps,
            max_angular_speed_radps,
            constraints,
            expires_at,
        })
    }
}

pub(crate) struct Api {
    localization: Subscriber<api::localize::LocalizationState>,
    map: Subscriber<api::map::Revision>,
    drive: Subscriber<api::drive::State>,
    batteries: Vec<BoundInput<api::component::battery::State>>,
    ranges: Vec<BoundInput<api::component::range::Sample>>,
    constraints: StatePublisher<api::safety::MotionConstraints>,
    state: StatePublisher<api::safety::State>,
}

pub(crate) struct SafetyState {
    inputs: WorldInputs,
    sequence: u64,
}

#[phoxal::service(state = SafetyState, api = Api)]
pub(crate) struct Safety;

impl Participant for Safety {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let robot = ctx.robot()?;
        let range_refs =
            robot.capability_refs(|capability| matches!(capability, Capability::Range(_)));
        let battery_refs =
            robot.capability_refs(|capability| matches!(capability, Capability::Battery(_)));

        let mut ranges = Vec::with_capacity(range_refs.len());
        for reference in &range_refs {
            ranges.push(BoundInput {
                reference: reference.clone(),
                samples: ctx
                    .subscriber(
                        api::topic::client()
                            .component(&reference.component_id)
                            .range(&reference.capability_id)
                            .sample(),
                    )
                    .await?,
            });
        }
        let mut batteries = Vec::with_capacity(battery_refs.len());
        for reference in &battery_refs {
            batteries.push(BoundInput {
                reference: reference.clone(),
                samples: ctx
                    .subscriber(
                        api::topic::client()
                            .component(&reference.component_id)
                            .battery(&reference.capability_id)
                            .state(),
                    )
                    .await?,
            });
        }
        Ok((
            SafetyState {
                inputs: WorldInputs::new(range_refs, battery_refs),
                sequence: 0,
            },
            Api {
                localization: ctx
                    .subscriber(api::topic::client().localize().state())
                    .await?,
                map: ctx
                    .subscriber(api::topic::client().map().revision())
                    .await?,
                drive: ctx.subscriber(api::topic::client().drive().state()).await?,
                batteries,
                ranges,
                constraints: ctx
                    .state_publisher(api::topic::owner().safety().constraints())
                    .await?,
                state: ctx
                    .state_publisher(api::topic::owner().safety().state())
                    .await?,
            },
        ))
    }

    fn reset(&self, _ctx: ResetContext, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        state.inputs.clear();
        state.sequence = 0;
        Ok(())
    }

    #[phoxal::step(hz = 10)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        retain_newest_stamped(&mut state.inputs.localization, &api.localization);
        retain_newest_stamped(&mut state.inputs.map, &api.map);
        retain_newest_stamped(&mut state.inputs.drive, &api.drive);
        for bound in &api.batteries {
            if let Some(slot) = state.inputs.batteries.get_mut(&bound.reference) {
                retain_newest_stamped(slot, &bound.samples);
            }
        }
        for bound in &api.ranges {
            if let Some(slot) = state.inputs.ranges.get_mut(&bound.reference) {
                retain_newest_stamped(slot, &bound.samples);
            }
        }

        // The map query is intentionally not performed from this synchronous
        // transition. Query admission and its snapshot handoff belong to the
        // managed-I/O/BusOwner follow-up; until that boundary exists, the
        // absence of a world-model result remains a typed fail-closed stop.
        state.inputs.drivable_space = None;

        state.sequence = state.sequence.saturating_add(1);
        let motion = state.inputs.assess(state.sequence, step.now())?;
        api.constraints.publish(&step.token, motion.clone())?;
        api.state.publish(
            &step.token,
            api::safety::State {
                clear: !motion.stop && motion.constraints.is_empty(),
                motion,
            },
        )?;
        Ok(())
    }
}

/// Consume everything buffered on `subscriber` and keep the newest sample that
/// expresses a robot instant.
///
/// A sample with no exact production instant expresses no robot time, so it can
/// never satisfy a freshness gate; dropping it here keeps the "missing means
/// fail closed" rule in one place. An empty drain leaves the retained sample
/// alone - it is the freshness gate, not the arrival of a newer sample, that
/// decides when a held value stops counting.
fn retain_newest_stamped<T: ContractBody>(slot: &mut Option<Timed<T>>, subscriber: &Subscriber<T>) {
    while let Some(observed) = subscriber.try_recv() {
        if let Some(at) = observed.metadata.produced_exactly_at() {
            *slot = Some(Timed::new(observed.body, at));
        }
    }
}

/// A sample is usable only if it belongs to this step's world history, is not
/// in its future, and is within the bound. A cross-timeline comparison is a
/// checked error, so it fails closed rather than silently passing.
fn usable<T>(sample: Option<&Timed<T>>, now: RobotInstant, stale: Duration) -> Option<&T> {
    sample
        .filter(|sample| sample.fresh_within(now, stale))
        .map(|sample| &sample.body)
}

#[cfg(test)]
fn submap_has_drivable_space(response: &api::map::SubmapResponse) -> Result<bool> {
    let expected = usize::try_from(response.width)
        .ok()
        .and_then(|width| {
            usize::try_from(response.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .context("map dimensions overflow")?;
    if expected == 0 || response.cells.len() != expected {
        bail!(
            "map submap shape mismatch: {}x{} requires {expected} cells, got {}",
            response.width,
            response.height,
            response.cells.len()
        );
    }
    if !response.resolution_m.is_finite() || response.resolution_m <= 0.0 {
        bail!("map submap resolution must be finite and positive");
    }
    if response
        .cells
        .iter()
        .any(|cell| *cell > 100 && *cell != 255)
    {
        bail!("map submap contains an invalid occupancy value");
    }
    Ok(response.cells.contains(&0))
}

fn stop_constraint(
    reason: api::safety::ConstraintReason,
    source: api::safety::ConstraintSource,
    observed_value: Option<f32>,
    now: RobotInstant,
    expires_at: RobotInstant,
) -> api::safety::Constraint {
    api::safety::Constraint {
        reason,
        source,
        stop: true,
        max_linear_speed_mps: Some(0.0),
        max_angular_speed_radps: Some(0.0),
        observed_value,
        valid_from: now,
        expires_at,
    }
}

fn limit_constraint(
    reason: api::safety::ConstraintReason,
    source: api::safety::ConstraintSource,
    max_linear_speed_mps: f32,
    observed_value: Option<f32>,
    now: RobotInstant,
    expires_at: RobotInstant,
) -> api::safety::Constraint {
    api::safety::Constraint {
        reason,
        source,
        stop: false,
        max_linear_speed_mps: Some(max_linear_speed_mps),
        max_angular_speed_radps: None,
        observed_value,
        valid_from: now,
        expires_at,
    }
}

fn source(
    kind: api::safety::ConstraintSourceKind,
    reference: Option<&CapabilityRef>,
) -> api::safety::ConstraintSource {
    let participant_id = match kind {
        api::safety::ConstraintSourceKind::Map => MAP_PARTICIPANT_ID,
        api::safety::ConstraintSourceKind::Localization => LOCALIZE_PARTICIPANT_ID,
        api::safety::ConstraintSourceKind::Drive => DRIVE_PARTICIPANT_ID,
        api::safety::ConstraintSourceKind::Battery => BATTERY_PARTICIPANT_ID,
        api::safety::ConstraintSourceKind::Range => reference
            .map_or(RANGE_PARTICIPANT_ID, |reference| {
                reference.component_id.as_str()
            }),
        api::safety::ConstraintSourceKind::WorldModel => WORLD_MODEL_PARTICIPANT_ID,
        api::safety::ConstraintSourceKind::Operator => OPERATOR_PARTICIPANT_ID,
    };
    api::safety::ConstraintSource {
        kind,
        participant_id: participant_id.to_string(),
        component_id: reference.map(|reference| reference.component_id.to_string()),
        capability_id: reference.map(|reference| reference.capability_id.to_string()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(seed: u64) -> TimelineId {
        TimelineId::from_raw(seed).expect("test timeline must be nonzero")
    }

    fn now() -> RobotInstant {
        RobotInstant::new(line(3), 2_000_000_000)
    }

    fn capability(reference: &str) -> CapabilityRef {
        reference
            .parse()
            .expect("a test capability reference is well formed")
    }

    fn range_ref() -> CapabilityRef {
        capability("front.range")
    }

    fn nominal_world() -> WorldInputs {
        let at = now();
        let mut world = WorldInputs::new([range_ref()], [capability("pack.battery")]);
        world.localization = Some(Timed::new(
            api::localize::LocalizationState {
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
                confidence: 1.0,
            },
            at,
        ));
        world.map = Some(Timed::new(
            api::map::Revision {
                revision: 1,
                resolution_m: 0.05,
            },
            at,
        ));
        world.drivable_space = Some(Timed::new(true, at));
        world.ranges.insert(
            range_ref(),
            Some(Timed::new(
                api::component::range::Sample {
                    distance_m: 2.0,
                    limits: None,
                    quality: None,
                    health: api::component::range::SensorHealth::Nominal,
                },
                at,
            )),
        );
        world
    }

    fn set_range_distance(world: &mut WorldInputs, distance_m: f32) {
        world
            .ranges
            .get_mut(&range_ref())
            .and_then(Option::as_mut)
            .expect("the nominal world declares a range sample")
            .body
            .distance_m = distance_m;
    }

    fn battery(charge_ratio: f32) -> Timed<api::component::battery::State> {
        Timed::new(
            api::component::battery::State {
                voltage_v: 16.0,
                current_a: 2.0,
                charge_ratio,
            },
            now(),
        )
    }

    /// A robot with several packs is only as safe as its emptiest one: a full
    /// pack must not mask a flat one sitting next to it.
    #[test]
    fn the_lowest_pack_decides_the_battery_constraint() {
        let mut world = nominal_world();
        world.batteries = BTreeMap::from([
            (capability("pack_a.battery"), Some(battery(1.0))),
            (capability("pack_b.battery"), Some(battery(0.04))),
        ]);

        let result = world.assess(1, now()).unwrap();
        assert!(result.stop);
        let constraint = result
            .constraints
            .iter()
            .find(|constraint| constraint.reason == api::safety::ConstraintReason::BatteryCritical)
            .expect("the flat pack must raise a critical-battery stop");
        assert_eq!(constraint.observed_value, Some(0.04));
    }

    #[test]
    fn a_stale_pack_is_ignored_rather_than_trusted() {
        let mut world = nominal_world();
        let mut stale = battery(0.04);
        stale.at = RobotInstant::new(line(3), 0);
        world.batteries = BTreeMap::from([(capability("pack.battery"), Some(stale))]);

        let result = world.assess(1, now()).unwrap();
        assert!(
            !result.constraints.iter().any(|constraint| {
                constraint.reason == api::safety::ConstraintReason::BatteryCritical
            }),
            "a pack that stopped reporting cannot keep asserting a charge level"
        );
    }

    #[test]
    fn nominal_world_is_clear_and_expires_after_three_periods() {
        let result = nominal_world().assess(7, now()).unwrap();
        assert!(!result.stop);
        assert!(result.constraints.is_empty());
        assert_eq!(result.sequence, 7);
        assert_eq!(
            result.expires_at.duration_since(now()).unwrap(),
            CONSTRAINT_TTL
        );
    }

    #[test]
    fn missing_world_inputs_fail_closed_with_typed_reasons() {
        let world = WorldInputs::new([range_ref()], [capability("pack.battery")]);
        let result = world.assess(1, now()).unwrap();
        assert!(result.stop);
        assert!(result.constraints.iter().any(|constraint| {
            constraint.reason == api::safety::ConstraintReason::LocalizationUnavailable
        }));
        assert!(result.constraints.iter().any(|constraint| {
            constraint.reason == api::safety::ConstraintReason::MapUnavailable
        }));
        assert!(result.constraints.iter().any(|constraint| {
            constraint.reason == api::safety::ConstraintReason::WorldUnavailable
                && constraint.source.component_id.as_deref() == Some("front")
        }));
    }

    /// The declared order that `Robot::capability_refs` guarantees is the order
    /// constraints are raised in, so a silent sensor is always reported at the
    /// same position and the nearest-obstacle tie-break resolves the same way.
    #[test]
    fn range_constraints_follow_the_declared_capability_order() {
        let world = WorldInputs::new(
            [
                capability("rear.range"),
                capability("front.b_range"),
                capability("front.a_range"),
            ],
            [],
        );
        let result = world.assess(1, now()).unwrap();
        let reported: Vec<_> = result
            .constraints
            .iter()
            .filter(|constraint| constraint.source.kind == api::safety::ConstraintSourceKind::Range)
            .map(|constraint| {
                (
                    constraint.source.component_id.clone(),
                    constraint.source.capability_id.clone(),
                )
            })
            .collect();
        assert_eq!(
            reported,
            vec![
                (Some("front".to_string()), Some("a_range".to_string())),
                (Some("front".to_string()), Some("b_range".to_string())),
                (Some("rear".to_string()), Some("range".to_string())),
            ]
        );
    }

    #[test]
    fn proximity_stops_or_limits_with_provenance() {
        let mut world = nominal_world();
        set_range_distance(&mut world, 0.2);
        let stopped = world.assess(1, now()).unwrap();
        assert!(stopped.stop);
        assert_eq!(
            stopped.constraints[0].reason,
            api::safety::ConstraintReason::ObstacleProximity
        );
        assert_eq!(
            stopped.constraints[0].source.component_id.as_deref(),
            Some("front")
        );

        set_range_distance(&mut world, 0.5);
        let limited = world.assess(2, now()).unwrap();
        assert!(!limited.stop);
        assert_eq!(
            limited.max_linear_speed_mps,
            Some(PROXIMITY_LINEAR_LIMIT_MPS)
        );
    }

    #[test]
    fn samples_from_a_retired_world_never_authorize_motion() {
        let mut world = nominal_world();
        world.localization.as_mut().unwrap().at = RobotInstant::new(line(2), now().ticks());
        let result = world.assess(1, now()).unwrap();
        assert!(result.stop);
        assert!(result.constraints.iter().any(|constraint| {
            constraint.reason == api::safety::ConstraintReason::LocalizationUnavailable
        }));
    }

    /// A world replacement retires every sample while keeping the declared
    /// capabilities, so each one must publish again before it counts.
    #[test]
    fn clearing_retires_every_sample_and_keeps_the_declared_capabilities() {
        let mut world = nominal_world();
        world.clear();
        let result = world.assess(1, now()).unwrap();
        assert!(result.stop);
        assert!(result.constraints.iter().any(|constraint| {
            constraint.reason == api::safety::ConstraintReason::WorldUnavailable
                && constraint.source.component_id.as_deref() == Some("front")
        }));
    }

    #[test]
    fn submap_content_is_validated_and_requires_known_free_space() {
        let response = api::map::SubmapResponse {
            width: 2,
            height: 1,
            resolution_m: 0.05,
            cells: vec![255, 0],
        };
        assert!(submap_has_drivable_space(&response).unwrap());
        assert!(
            !submap_has_drivable_space(&api::map::SubmapResponse {
                cells: vec![255, 100],
                ..response.clone()
            })
            .unwrap()
        );
        assert!(
            submap_has_drivable_space(&api::map::SubmapResponse {
                cells: vec![0],
                ..response
            })
            .is_err()
        );
    }
}
