//! `safety` - world-input assessment into typed, expiring motion constraints.
//!
//! This is intentionally not the deleted authorization-envelope runtime.
//! Emergency stop remains owned by `motion`; safety consumes localization, map,
//! proximity, drive, and optional battery state and emits one diagnosable
//! constraint product. Autonomous motion treats a missing, stale, future-dated,
//! or retired-timeline product as a stop. Manual recovery can operate without that
//! product, while any valid protective stop or limit still applies.

use anyhow::{Context, Result, bail};
use std::time::Duration;

use phoxal::api;
use phoxal::model::component::v0::capability::Capability;
use phoxal::model::v0::Robot;
use phoxal::prelude::*;

const INPUT_STALE: Duration = Duration::from_nanos(1_000_000_000);
const MAP_STALE: Duration = Duration::from_nanos(600_000_000);
const CONSTRAINT_TTL: Duration = Duration::from_millis(300);
const MIN_LOCALIZATION_CONFIDENCE: f32 = 0.25;
const PROTECTIVE_STOP_DISTANCE_M: f32 = 0.25;
const PROXIMITY_LIMIT_DISTANCE_M: f32 = 0.60;
const PROXIMITY_LINEAR_LIMIT_MPS: f32 = 0.15;

#[derive(Clone)]
struct Timed<T> {
    body: T,
    at: RobotInstant,
}

#[derive(Clone)]
struct RangeBinding {
    component_id: String,
    capability_id: String,
}

struct WorldInputs {
    localization: Option<Timed<api::localize::LocalizationState>>,
    map: Option<Timed<api::map::Revision>>,
    drivable_space: Option<Timed<bool>>,
    drive: Option<Timed<api::drive::State>>,
    battery: Option<Timed<api::battery::State>>,
    ranges: Vec<Option<Timed<api::component::range::Sample>>>,
}

impl WorldInputs {
    fn new(range_count: usize) -> Self {
        Self {
            localization: None,
            map: None,
            drivable_space: None,
            drive: None,
            battery: None,
            ranges: vec![None; range_count],
        }
    }
}

#[derive(phoxal::Api)]
pub struct Api {
    localization: Subscriber<api::localize::LocalizationState>,
    map: Subscriber<api::map::Revision>,
    map_submap: Querier<api::map::SubmapRequest, api::map::SubmapResponse>,
    drive: Subscriber<api::drive::State>,
    battery: Subscriber<api::battery::State>,
    ranges: Vec<Subscriber<api::component::range::Sample>>,
    constraints: StatePublisher<api::safety::MotionConstraints>,
    state: StatePublisher<api::safety::State>,
}

#[phoxal::service(id = "safety", config = ())]
pub struct Safety {
    bindings: Vec<RangeBinding>,
    inputs: WorldInputs,
    sequence: u64,
}

#[phoxal::behavior]
impl Safety {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let cap = ctx.owner_capability();
        let bindings = range_bindings(ctx.robot()?);
        let mut ranges = Vec::with_capacity(bindings.len());
        for binding in &bindings {
            ranges.push(
                ctx.subscriber(
                    api::topic::new()
                        .component(&binding.component_id)
                        .range(&binding.capability_id)
                        .sample(),
                    32,
                )
                .await?,
            );
        }
        Ok((
            Self {
                inputs: WorldInputs::new(bindings.len()),
                bindings,
                sequence: 0,
            },
            Self::Api {
                localization: ctx
                    .subscriber(api::topic::new().localize().state(), 32)
                    .await?,
                map: ctx
                    .subscriber(api::topic::new().map().revision(), 32)
                    .await?,
                map_submap: ctx.querier(api::topic::new().map().submap()).await?,
                drive: ctx
                    .subscriber(api::topic::new().drive().state(), 32)
                    .await?,
                battery: ctx
                    .subscriber(api::topic::new().battery().state(), 32)
                    .await?,
                ranges,
                constraints: ctx
                    .state_publisher(api::topic::internal::new(cap).safety().constraints())
                    .await?,
                state: ctx
                    .state_publisher(api::topic::internal::new(cap).safety().state())
                    .await?,
            },
        ))
    }

    #[reset]
    async fn reset(&mut self, _ctx: ResetContext) -> Result<()> {
        self.inputs = WorldInputs::new(self.bindings.len());
        self.sequence = 0;
        Ok(())
    }

    #[step(hz = 10)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        drain_latest(&mut self.inputs.localization, &api.localization);
        drain_latest(&mut self.inputs.map, &api.map);
        drain_latest(&mut self.inputs.drive, &api.drive);
        drain_latest(&mut self.inputs.battery, &api.battery);
        for (slot, subscriber) in self.inputs.ranges.iter_mut().zip(&api.ranges) {
            drain_latest(slot, subscriber);
        }

        self.inputs.drivable_space = if let Some(localization) =
            usable(self.inputs.localization.as_ref(), step.now(), INPUT_STALE)
        {
            let radius = 0.20;
            let response = api
                .map_submap
                .query(api::map::SubmapRequest {
                    min_x_m: localization.x_m - radius,
                    min_y_m: localization.y_m - radius,
                    max_x_m: localization.x_m + radius,
                    max_y_m: localization.y_m + radius,
                })
                .await;
            match response {
                Ok(response) => Some(Timed {
                    body: submap_has_drivable_space(&response)?,
                    at: step.now(),
                }),
                Err(_) => None,
            }
        } else {
            None
        };

        self.sequence = self.sequence.saturating_add(1);
        let motion = assess(&self.inputs, &self.bindings, self.sequence, step.now())?;
        api.constraints.publish(step.token(), motion.clone())?;
        api.state.publish(
            step.token(),
            api::safety::State {
                clear: !motion.stop && motion.constraints.is_empty(),
                motion,
            },
        )?;
        Ok(())
    }
}

fn drain_latest<T: phoxal::bus::ContractBody + Clone>(
    slot: &mut Option<Timed<T>>,
    subscriber: &Subscriber<T>,
) {
    while let Some(observed) = subscriber.try_recv() {
        // A sample with no exact production instant expresses no robot time, so
        // it can never satisfy a freshness gate; dropping it here keeps the
        // "missing means fail closed" rule in one place.
        if let Some(at) = observed.metadata.produced_exactly_at() {
            *slot = Some(Timed {
                body: observed.body,
                at,
            });
        }
    }
}

fn assess(
    world: &WorldInputs,
    bindings: &[RangeBinding],
    sequence: u64,
    now: RobotInstant,
) -> Result<api::safety::MotionConstraints> {
    let expires_at = now.saturating_add(CONSTRAINT_TTL);
    let mut constraints = Vec::new();

    match usable(world.localization.as_ref(), now, INPUT_STALE) {
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

    if usable(world.map.as_ref(), now, MAP_STALE).is_none() {
        constraints.push(stop_constraint(
            api::safety::ConstraintReason::MapUnavailable,
            source(api::safety::ConstraintSourceKind::Map, None),
            None,
            now,
            expires_at,
        ));
    }

    match usable(world.drivable_space.as_ref(), now, MAP_STALE) {
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

    let mut nearest_range = None::<(f32, &RangeBinding)>;
    for (binding, sample) in bindings.iter().zip(&world.ranges) {
        let Some(sample) = usable(sample.as_ref(), now, INPUT_STALE) else {
            constraints.push(stop_constraint(
                api::safety::ConstraintReason::WorldUnavailable,
                source(api::safety::ConstraintSourceKind::Range, Some(binding)),
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
                source(api::safety::ConstraintSourceKind::Range, Some(binding)),
                Some(sample.distance_m),
                now,
                expires_at,
            ));
            continue;
        }
        if nearest_range.is_none_or(|(distance, _)| sample.distance_m < distance) {
            nearest_range = Some((sample.distance_m, binding));
        }
    }

    if let Some((distance, binding)) = nearest_range {
        if distance <= PROTECTIVE_STOP_DISTANCE_M {
            constraints.push(stop_constraint(
                api::safety::ConstraintReason::ObstacleProximity,
                source(api::safety::ConstraintSourceKind::Range, Some(binding)),
                Some(distance),
                now,
                expires_at,
            ));
        } else if distance <= PROXIMITY_LIMIT_DISTANCE_M {
            constraints.push(limit_constraint(
                api::safety::ConstraintReason::ObstacleProximity,
                source(api::safety::ConstraintSourceKind::Range, Some(binding)),
                PROXIMITY_LINEAR_LIMIT_MPS,
                Some(distance),
                now,
                expires_at,
            ));
        }
    }

    if let Some(drive) = usable(world.drive.as_ref(), now, INPUT_STALE)
        && matches!(
            drive.stop_reason,
            Some(api::drive::StopReason::Fault | api::drive::StopReason::ActuatorCommandNotFinite)
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

    if let Some(battery) = usable(world.battery.as_ref(), now, INPUT_STALE) {
        if !battery.charge_ratio.is_finite() {
            bail!("battery world input contains a non-finite charge ratio");
        }
        if battery.charge_ratio <= 0.05 {
            constraints.push(stop_constraint(
                api::safety::ConstraintReason::BatteryCritical,
                source(api::safety::ConstraintSourceKind::Battery, None),
                Some(battery.charge_ratio),
                now,
                expires_at,
            ));
        } else if battery.charge_ratio <= 0.15 {
            constraints.push(limit_constraint(
                api::safety::ConstraintReason::BatteryLow,
                source(api::safety::ConstraintSourceKind::Battery, None),
                PROXIMITY_LINEAR_LIMIT_MPS,
                Some(battery.charge_ratio),
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

/// A sample is usable only if it belongs to this step's world history, is not
/// in its future, and is within the bound. A cross-timeline comparison is a
/// checked error, so it fails closed rather than silently passing.
fn usable<T>(sample: Option<&Timed<T>>, now: RobotInstant, stale: Duration) -> Option<&T> {
    sample
        .filter(|sample| {
            TimeWindow::exact(sample.at)
                .possibly_fresh_within(now, stale)
                .unwrap_or(false)
        })
        .map(|sample| &sample.body)
}

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
    binding: Option<&RangeBinding>,
) -> api::safety::ConstraintSource {
    let participant_id = match kind {
        api::safety::ConstraintSourceKind::Map => "map",
        api::safety::ConstraintSourceKind::Localization => "localize",
        api::safety::ConstraintSourceKind::Drive => "drive",
        api::safety::ConstraintSourceKind::Battery => "battery-provider",
        api::safety::ConstraintSourceKind::Range => binding
            .map(|binding| binding.component_id.as_str())
            .unwrap_or("range-provider"),
        api::safety::ConstraintSourceKind::WorldModel => "safety",
        api::safety::ConstraintSourceKind::Operator => "operator",
    };
    api::safety::ConstraintSource {
        kind,
        participant_id: participant_id.to_string(),
        component_id: binding.map(|binding| binding.component_id.clone()),
        capability_id: binding.map(|binding| binding.capability_id.clone()),
    }
}

fn range_bindings(robot: &Robot) -> Vec<RangeBinding> {
    let mut bindings = robot
        .manifest
        .components()
        .iter()
        .filter_map(|(component_id, instance)| {
            robot
                .components
                .get(&instance.component)
                .map(|component| (component_id, component))
        })
        .flat_map(|(component_id, component)| {
            component
                .capabilities
                .iter()
                .filter(|(_, capability)| matches!(capability, Capability::Range(_)))
                .map(|(capability_id, _)| RangeBinding {
                    component_id: component_id.clone(),
                    capability_id: capability_id.clone(),
                })
        })
        .collect::<Vec<_>>();
    bindings.sort_by(|left, right| {
        left.component_id
            .cmp(&right.component_id)
            .then_with(|| left.capability_id.cmp(&right.capability_id))
    });
    bindings
}

#[cfg(test)]
mod tests {
    fn line(seed: u64) -> phoxal::bus::TimelineId {
        phoxal::bus::TimelineId::from_raw(seed).expect("test timeline must be nonzero")
    }

    use super::*;

    fn now() -> RobotInstant {
        RobotInstant::new(line(3), 2_000_000_000)
    }

    fn nominal_world() -> (Vec<RangeBinding>, WorldInputs) {
        let binding = RangeBinding {
            component_id: "front".to_string(),
            capability_id: "range".to_string(),
        };
        let at = now();
        let mut world = WorldInputs::new(1);
        world.localization = Some(Timed {
            body: api::localize::LocalizationState {
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
                confidence: 1.0,
            },
            at,
        });
        world.map = Some(Timed {
            body: api::map::Revision {
                revision: 1,
                resolution_m: 0.05,
            },
            at,
        });
        world.drivable_space = Some(Timed { body: true, at });
        world.ranges[0] = Some(Timed {
            body: api::component::range::Sample {
                distance_m: 2.0,
                limits: None,
                quality: None,
                health: api::component::range::SensorHealth::Nominal,
            },
            at,
        });
        (vec![binding], world)
    }

    #[test]
    fn nominal_world_is_clear_and_expires_after_three_periods() {
        let (bindings, world) = nominal_world();
        let result = assess(&world, &bindings, 7, now()).unwrap();
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
        let bindings = vec![RangeBinding {
            component_id: "front".to_string(),
            capability_id: "range".to_string(),
        }];
        let result = assess(&WorldInputs::new(1), &bindings, 1, now()).unwrap();
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

    #[test]
    fn proximity_stops_or_limits_with_provenance() {
        let (bindings, mut world) = nominal_world();
        world.ranges[0].as_mut().unwrap().body.distance_m = 0.2;
        let stopped = assess(&world, &bindings, 1, now()).unwrap();
        assert!(stopped.stop);
        assert_eq!(
            stopped.constraints[0].reason,
            api::safety::ConstraintReason::ObstacleProximity
        );
        assert_eq!(
            stopped.constraints[0].source.component_id.as_deref(),
            Some("front")
        );

        world.ranges[0].as_mut().unwrap().body.distance_m = 0.5;
        let limited = assess(&world, &bindings, 2, now()).unwrap();
        assert!(!limited.stop);
        assert_eq!(
            limited.max_linear_speed_mps,
            Some(PROXIMITY_LINEAR_LIMIT_MPS)
        );
    }

    #[test]
    fn samples_from_a_retired_world_never_authorize_motion() {
        let (bindings, mut world) = nominal_world();
        world.localization.as_mut().unwrap().at = RobotInstant::new(line(2), now().ticks());
        let result = assess(&world, &bindings, 1, now()).unwrap();
        assert!(result.stop);
        assert!(result.constraints.iter().any(|constraint| {
            constraint.reason == api::safety::ConstraintReason::LocalizationUnavailable
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
