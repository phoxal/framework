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
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;
use tokio::sync::mpsc;

use phoxal::api;
use phoxal::bus::{ContractBody, SampleDeliveryContract, StateDeliveryContract};
use phoxal::model::FootprintEnvelope;
use phoxal::model::component::capability::{Capability, CapabilityRole};
use phoxal::model::identity::CapabilityRef;
use phoxal::prelude::*;

const INPUT_STALE: Duration = Duration::from_nanos(1_000_000_000);
const MAP_STALE: Duration = Duration::from_nanos(600_000_000);
const CONSTRAINT_TTL: Duration = Duration::from_millis(300);
const MAP_QUERY_PERIOD: Duration = Duration::from_millis(100);
const MAP_QUERY_TIMEOUT: Duration = Duration::from_millis(250);
const MAP_FRAME: &str = "map";
const MAP_QUERY_BOUNDS: (f64, f64, f64, f64) = (0.0, 0.0, 3.2, 3.2);
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
struct BoundSampleInput<B> {
    reference: CapabilityRef,
    samples: SampleReceiver<B>,
}

struct BoundStateInput<B> {
    reference: CapabilityRef,
    samples: StateView<B>,
}

/// Health evidence retained from the runner-owned map query loop. A failed
/// query is observable immediately, while a closed channel/terminal loop is
/// retained as a separate terminal fact so a later stale success cannot revive
/// the participant accidentally.
#[derive(Clone, Debug, Default)]
struct MapHealth {
    healthy: bool,
    stale: bool,
    partial: bool,
    terminal: bool,
    detail: Option<String>,
}

enum MapQueryEvent {
    Snapshot {
        epoch: u64,
        response: api::map::SubmapResponse,
        completed_at: LocalInstant,
    },
    Unhealthy {
        epoch: u64,
        detail: String,
    },
    Terminal {
        epoch: u64,
        detail: String,
    },
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
    map_window: Option<Timed<api::map::GridWindow>>,
    map_health: MapHealth,
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
            map_window: None,
            map_health: MapHealth::default(),
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
        self.map_window = None;
        self.map_health = MapHealth::default();
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
    fn assess(
        &self,
        sequence: u64,
        now: RobotInstant,
        footprint: Option<FootprintEnvelope>,
    ) -> Result<api::safety::MotionConstraints> {
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

        match footprint {
            None => constraints.push(stop_constraint(
                api::safety::ConstraintReason::FootprintUnavailable,
                source(api::safety::ConstraintSourceKind::Footprint, None),
                None,
                now,
                expires_at,
            )),
            Some(footprint) => {
                if let Err(reason) = self.map_is_safe(&footprint, now) {
                    constraints.push(stop_constraint(
                        reason,
                        source(api::safety::ConstraintSourceKind::Map, None),
                        None,
                        now,
                        expires_at,
                    ));
                }
            }
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
        for (reference, sample) in &self.batteries {
            let Some(sample) = sample.as_ref() else {
                constraints.push(stop_constraint(
                    api::safety::ConstraintReason::BatteryUnavailable,
                    source(api::safety::ConstraintSourceKind::Battery, Some(reference)),
                    None,
                    now,
                    expires_at,
                ));
                continue;
            };
            if !sample.fresh_within(now, INPUT_STALE) {
                constraints.push(stop_constraint(
                    api::safety::ConstraintReason::BatteryStale,
                    source(api::safety::ConstraintSourceKind::Battery, Some(reference)),
                    None,
                    now,
                    expires_at,
                ));
                continue;
            }
            let battery = &sample.body;
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

        let stopped = constraints
            .iter()
            .any(|constraint| matches!(constraint, api::safety::Constraint::Stopped { .. }));
        let max_linear_speed_mps = constraints
            .iter()
            .filter_map(|constraint| match constraint {
                api::safety::Constraint::Limited {
                    max_linear_speed_mps,
                    ..
                } => Some(*max_linear_speed_mps),
                api::safety::Constraint::Stopped { .. } => None,
            })
            .reduce(f32::min);
        let max_angular_speed_radps = constraints
            .iter()
            .filter_map(|constraint| match constraint {
                api::safety::Constraint::Limited {
                    max_angular_speed_radps,
                    ..
                } => Some(*max_angular_speed_radps),
                api::safety::Constraint::Stopped { .. } => None,
            })
            .reduce(f32::min);
        let permission = if stopped {
            api::safety::MotionPermission::Stopped {
                reasons: constraints.iter().map(constraint_reason).collect(),
            }
        } else if constraints.is_empty() {
            api::safety::MotionPermission::Clear
        } else {
            api::safety::MotionPermission::Limited {
                effective_linear_speed_mps: max_linear_speed_mps.unwrap_or(f32::MAX),
                effective_angular_speed_radps: max_angular_speed_radps.unwrap_or(f32::MAX),
                reasons: constraints.iter().map(constraint_reason).collect(),
            }
        };
        Ok(api::safety::MotionConstraints {
            sequence,
            permission,
            constraints,
            expires_at,
        })
    }

    fn map_is_safe(
        &self,
        footprint: &FootprintEnvelope,
        now: RobotInstant,
    ) -> std::result::Result<(), api::safety::ConstraintReason> {
        let Some(window_sample) = self.map_window.as_ref() else {
            return Err(api::safety::ConstraintReason::MapUnavailable);
        };
        if !window_sample.fresh_within(now, MAP_STALE) {
            return Err(api::safety::ConstraintReason::MapStale);
        }
        let window = &window_sample.body;
        if self.map_health.terminal {
            return Err(api::safety::ConstraintReason::MapUnavailable);
        }
        if self.map_health.partial {
            return Err(api::safety::ConstraintReason::MapPartial);
        }
        if !self.map_health.healthy {
            return Err(api::safety::ConstraintReason::MapUnavailable);
        }
        let Some(revision) = usable(self.map.as_ref(), now, MAP_STALE) else {
            return Err(api::safety::ConstraintReason::MapRevisionInvalid);
        };
        if revision.revision != window.revision
            || !revision.resolution_m.is_finite()
            || revision.resolution_m <= 0.0
            || window.frame_id != MAP_FRAME
            || !window.resolution_m.is_finite()
            || window.resolution_m <= 0.0
            || (window.resolution_m - revision.resolution_m).abs() > f32::EPSILON
            || !window.origin_pose.x_m.is_finite()
            || !window.origin_pose.y_m.is_finite()
            || !window.origin_pose.yaw_rad.is_finite()
            || window.origin_pose.yaw_rad.abs() > f64::EPSILON
            || !window.cell_origin.x_m.is_finite()
            || !window.cell_origin.y_m.is_finite()
            || !bounds_are_finite_and_positive(&window.requested)
            || !bounds_are_finite_and_positive(&window.covered)
            || !bounds_complete(window)
        {
            return Err(api::safety::ConstraintReason::MapRevisionInvalid);
        }
        let Some(localization) = usable(self.localization.as_ref(), now, INPUT_STALE) else {
            return Err(api::safety::ConstraintReason::LocalizationUnavailable);
        };
        if !localization.x_m.is_finite() || !localization.y_m.is_finite() {
            return Err(api::safety::ConstraintReason::LocalizationUnavailable);
        }
        let required = footprint.required_radius_m();
        if !required.is_finite() || required <= 0.0 {
            return Err(api::safety::ConstraintReason::FootprintMismatch);
        }
        let resolution = f64::from(window.resolution_m);
        let width = usize::try_from(window.width).ok();
        let height = usize::try_from(window.height).ok();
        let Some((width, height)) = width.zip(height) else {
            return Err(api::safety::ConstraintReason::FootprintMismatch);
        };
        let Some(expected) = width.checked_mul(height) else {
            return Err(api::safety::ConstraintReason::FootprintMismatch);
        };
        if expected == 0 || window.cells.len() != expected {
            return Err(api::safety::ConstraintReason::FootprintMismatch);
        }

        let width_m = (window.width as f64) * resolution;
        let height_m = (window.height as f64) * resolution;
        let epsilon = resolution * 1.0e-6;
        if !width_m.is_finite()
            || !height_m.is_finite()
            || (window.origin_pose.x_m - window.cell_origin.x_m).abs() > epsilon
            || (window.origin_pose.y_m - window.cell_origin.y_m).abs() > epsilon
            || (window.covered.max_x_m - window.covered.min_x_m - width_m).abs() > epsilon
            || (window.covered.max_y_m - window.covered.min_y_m - height_m).abs() > epsilon
            || (window.cell_origin.x_m - window.covered.min_x_m).abs() > epsilon
            || (window.cell_origin.y_m - window.covered.min_y_m).abs() > epsilon
        {
            return Err(api::safety::ConstraintReason::FootprintMismatch);
        }

        let min_x = localization.x_m - required;
        let max_x = localization.x_m + required;
        let min_y = localization.y_m - required;
        let max_y = localization.y_m + required;
        let covered = &window.covered;
        if min_x < covered.min_x_m
            || max_x > covered.max_x_m
            || min_y < covered.min_y_m
            || max_y > covered.max_y_m
        {
            return Err(api::safety::ConstraintReason::FootprintMismatch);
        }

        if required > f64::MAX.sqrt() {
            return Err(api::safety::ConstraintReason::FootprintMismatch);
        }
        let required_squared = required * required;
        let mut checked = 0usize;
        for y in 0..height {
            for x in 0..width {
                // Include a cell whenever its square intersects the compiled
                // radial envelope. Testing the nearest point on the square,
                // rather than only its centre, keeps an obstacle on the edge
                // from being mistaken for clearance.
                let cell_min_x = window.cell_origin.x_m + x as f64 * resolution;
                let cell_min_y = window.cell_origin.y_m + y as f64 * resolution;
                let cell_max_x = cell_min_x + resolution;
                let cell_max_y = cell_min_y + resolution;
                let nearest_x = localization.x_m.clamp(cell_min_x, cell_max_x);
                let nearest_y = localization.y_m.clamp(cell_min_y, cell_max_y);
                let dx = nearest_x - localization.x_m;
                let dy = nearest_y - localization.y_m;
                if dx * dx + dy * dy <= required_squared {
                    checked += 1;
                    match window.cells[y * width + x] {
                        api::map::Occupancy::Free => {}
                        api::map::Occupancy::Unknown => {
                            return Err(api::safety::ConstraintReason::UnknownOccupancy);
                        }
                        api::map::Occupancy::Occupied => {
                            return Err(api::safety::ConstraintReason::FootprintObstacle);
                        }
                    }
                }
            }
        }
        (checked > 0)
            .then_some(())
            .ok_or(api::safety::ConstraintReason::FootprintMismatch)
    }
}

pub(crate) struct Api {
    localization: StateView<api::localize::LocalizationState>,
    map: StateView<api::map::Revision>,
    map_events: std::sync::Mutex<mpsc::Receiver<MapQueryEvent>>,
    map_epoch: Arc<AtomicU64>,
    drive: StateView<api::drive::State>,
    batteries: Vec<BoundStateInput<api::component::battery::State>>,
    ranges: Vec<BoundSampleInput<api::component::range::Sample>>,
    constraints: StatePublisher<api::safety::MotionConstraints>,
    state: StatePublisher<api::safety::State>,
}

pub(crate) struct SafetyState {
    inputs: WorldInputs,
    sequence: u64,
    footprint: Option<FootprintEnvelope>,
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
        let footprint = robot.footprint_envelope();
        let safety_refs = robot.capabilities_with_role(CapabilityRole::Safety);
        let range_refs = safety_refs
            .iter()
            .filter(|reference| matches!(robot.capability(reference), Some(Capability::Range(_))))
            .cloned()
            .collect::<Vec<_>>();
        let battery_refs = safety_refs
            .iter()
            .filter(|reference| matches!(robot.capability(reference), Some(Capability::Battery(_))))
            .cloned()
            .collect::<Vec<_>>();

        let mut ranges = Vec::with_capacity(range_refs.len());
        for reference in &range_refs {
            ranges.push(BoundSampleInput {
                reference: reference.clone(),
                samples: ctx
                    .sample_receiver(
                        api::topic::client()
                            .component(&reference.component_id)?
                            .range(&reference.capability_id)?
                            .sample(),
                    )
                    .await?,
            });
        }
        let mut batteries = Vec::with_capacity(battery_refs.len());
        for reference in &battery_refs {
            batteries.push(BoundStateInput {
                reference: reference.clone(),
                samples: ctx
                    .state_view(
                        api::topic::client()
                            .component(&reference.component_id)?
                            .battery(&reference.capability_id)?
                            .state(),
                    )
                    .await?,
            });
        }
        let map_query = ctx.querier(api::topic::client().map().submap())?;
        let (map_sender, map_events) = mpsc::channel(4);
        let map_epoch = Arc::new(AtomicU64::new(0));
        let task_epoch = Arc::clone(&map_epoch);
        let task_query = map_query.clone();
        ctx.spawn_managed("safety-map-query", async move {
            loop {
                let epoch = task_epoch.load(Ordering::Acquire);
                let request = api::map::SubmapRequest {
                    min_x_m: MAP_QUERY_BOUNDS.0,
                    min_y_m: MAP_QUERY_BOUNDS.1,
                    max_x_m: MAP_QUERY_BOUNDS.2,
                    max_y_m: MAP_QUERY_BOUNDS.3,
                };
                let outcome =
                    tokio::time::timeout(MAP_QUERY_TIMEOUT, task_query.query(request)).await;
                let (event, terminal) = match outcome {
                    Ok(Ok(response)) => {
                        let Some(completed_at) = LocalInstant::try_now() else {
                            return Err(anyhow::anyhow!(
                                "safety map query task could not read the host boot clock"
                            ));
                        };
                        (
                            MapQueryEvent::Snapshot {
                                epoch,
                                response,
                                completed_at,
                            },
                            false,
                        )
                    }
                    Ok(Err(error)) => {
                        let terminal = map_query_is_terminal(&error);
                        let detail = error.to_string();
                        (
                            if terminal {
                                MapQueryEvent::Terminal { epoch, detail }
                            } else {
                                MapQueryEvent::Unhealthy { epoch, detail }
                            },
                            terminal,
                        )
                    }
                    Err(_elapsed) => (
                        MapQueryEvent::Unhealthy {
                            epoch,
                            detail: "bounded map query timed out".to_string(),
                        },
                        false,
                    ),
                };
                if map_sender.send(event).await.is_err() {
                    return Err(anyhow::anyhow!(
                        "safety map query task lost its snapshot channel"
                    ));
                }
                if terminal {
                    return Err(anyhow::anyhow!(
                        "safety map query reached a terminal protocol failure"
                    ));
                }
                tokio::time::sleep(MAP_QUERY_PERIOD).await;
            }
        });
        Ok((
            SafetyState {
                inputs: WorldInputs::new(range_refs, battery_refs),
                sequence: 0,
                footprint,
            },
            Api {
                localization: ctx
                    .state_view(api::topic::client().localize().state())
                    .await?,
                map: ctx
                    .state_view(api::topic::client().map().revision())
                    .await?,
                map_events: std::sync::Mutex::new(map_events),
                map_epoch,
                drive: ctx.state_view(api::topic::client().drive().state()).await?,
                batteries,
                ranges,
                constraints: ctx.state_publisher(api::topic::owner().safety().constraints())?,
                state: ctx.state_publisher(api::topic::owner().safety().state())?,
            },
        ))
    }

    fn reset(&self, _ctx: ResetContext, api: &Self::Api, state: &mut Self::State) -> Result<()> {
        api.map_epoch.fetch_add(1, Ordering::AcqRel);
        if let Ok(mut events) = api.map_events.try_lock() {
            while events.try_recv().is_ok() {}
        }
        state.inputs.clear();
        state.sequence = 0;
        Ok(())
    }

    #[phoxal::step(hz = 10)]
    fn step(&self, api: &Self::Api, step: StepContext, state: &mut Self::State) -> Result<()> {
        let now = step.now();
        let Some(host_now) = LocalInstant::try_now() else {
            state.inputs.map_health = MapHealth {
                healthy: false,
                stale: true,
                partial: false,
                terminal: true,
                detail: Some("the host boot clock could not be read".to_string()),
            };
            bail!("the host boot clock could not be read");
        };
        retain_newest_view(&mut state.inputs.localization, &api.localization);
        retain_newest_view(&mut state.inputs.map, &api.map);
        retain_newest_view(&mut state.inputs.drive, &api.drive);
        for bound in &api.batteries {
            if let Some(slot) = state.inputs.batteries.get_mut(&bound.reference) {
                retain_newest_view(slot, &bound.samples);
            }
        }
        for bound in &api.ranges {
            if let Some(slot) = state.inputs.ranges.get_mut(&bound.reference) {
                retain_newest_stamped(slot, &bound.samples);
            }
        }

        // Query IO is runner-owned. This synchronous transition only drains
        // already-retained results and never awaits or issues a query itself.
        let mut map_events = api.map_events.try_lock().ok();
        while let Some(Ok(event)) = map_events.as_mut().map(|receiver| receiver.try_recv()) {
            let current_epoch = api.map_epoch.load(Ordering::Acquire);
            match event {
                MapQueryEvent::Snapshot { epoch, .. } if epoch != current_epoch => {
                    continue;
                }
                MapQueryEvent::Unhealthy { epoch, .. } | MapQueryEvent::Terminal { epoch, .. }
                    if epoch != current_epoch =>
                {
                    continue;
                }
                MapQueryEvent::Snapshot {
                    response,
                    completed_at,
                    ..
                } => {
                    if !map_snapshot_is_fresh(completed_at, host_now) {
                        state.inputs.map_health = MapHealth {
                            healthy: false,
                            stale: true,
                            partial: false,
                            terminal: false,
                            detail: Some("map query result became stale before step".to_string()),
                        };
                        continue;
                    }
                    match response {
                        api::map::SubmapResponse::Window(window) => {
                            state.inputs.map_window = Some(Timed::new(window, now));
                            if !state.inputs.map_health.terminal {
                                state.inputs.map_health = MapHealth {
                                    healthy: true,
                                    stale: false,
                                    partial: false,
                                    terminal: false,
                                    detail: None,
                                };
                            }
                        }
                        api::map::SubmapResponse::Partial { window } => {
                            state.inputs.map_window = Some(Timed::new(window, now));
                            state.inputs.map_health = MapHealth {
                                healthy: false,
                                stale: false,
                                partial: true,
                                terminal: false,
                                detail: Some("map query returned a partial window".to_string()),
                            };
                        }
                        api::map::SubmapResponse::OutOfBounds { .. } => {
                            state.inputs.map_health = MapHealth {
                                healthy: false,
                                stale: false,
                                partial: true,
                                terminal: false,
                                detail: Some("map query was out of bounds".to_string()),
                            };
                        }
                    }
                }
                MapQueryEvent::Unhealthy { detail, .. } => {
                    state.inputs.map_health.healthy = false;
                    state.inputs.map_health.stale = false;
                    state.inputs.map_health.detail = Some(detail);
                }
                MapQueryEvent::Terminal { detail, .. } => {
                    state.inputs.map_health.healthy = false;
                    state.inputs.map_health.terminal = true;
                    state.inputs.map_health.detail = Some(detail);
                }
            }
        }
        if map_events
            .as_ref()
            .is_some_and(|receiver| receiver.is_closed())
        {
            state.inputs.map_health.healthy = false;
            state.inputs.map_health.terminal = true;
            state.inputs.map_health.detail = Some("map query channel closed".to_string());
        }

        state.sequence = state.sequence.saturating_add(1);
        let motion = state.inputs.assess(state.sequence, now, state.footprint)?;
        api.constraints.publish(&step.token, motion.clone())?;
        api.state.publish(
            &step.token,
            api::safety::State {
                constraints: motion,
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
fn retain_newest_stamped<T: ContractBody + SampleDeliveryContract>(
    slot: &mut Option<Timed<T>>,
    subscriber: &SampleReceiver<T>,
) {
    while let Some(observed) = subscriber.try_recv() {
        if let Some(at) = observed.metadata.produced_exactly_at() {
            *slot = Some(Timed::new(observed.body, at));
        }
    }
}

fn retain_newest_view<T: ContractBody + Clone + StateDeliveryContract>(
    slot: &mut Option<Timed<T>>,
    view: &StateView<T>,
) {
    if let Some(observed) = view.observed()
        && let Some(at) = observed.metadata.produced_exactly_at()
    {
        *slot = Some(Timed::new(observed.body.clone(), at));
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

fn map_snapshot_is_fresh(completed_at: LocalInstant, now: LocalInstant) -> bool {
    now.saturating_duration_since(completed_at) <= MAP_STALE
}

fn bounds_complete(window: &api::map::GridWindow) -> bool {
    let epsilon = f64::from(window.resolution_m) * 1.0e-6;
    (window.requested.min_x_m - window.covered.min_x_m).abs() <= epsilon
        && (window.requested.min_y_m - window.covered.min_y_m).abs() <= epsilon
        && (window.requested.max_x_m - window.covered.max_x_m).abs() <= epsilon
        && (window.requested.max_y_m - window.covered.max_y_m).abs() <= epsilon
}

fn bounds_are_finite_and_positive(bounds: &api::map::Bounds) -> bool {
    [
        bounds.min_x_m,
        bounds.min_y_m,
        bounds.max_x_m,
        bounds.max_y_m,
    ]
    .into_iter()
    .all(f64::is_finite)
        && bounds.min_x_m < bounds.max_x_m
        && bounds.min_y_m < bounds.max_y_m
}

fn map_query_is_terminal(error: &phoxal::bus::QueryError) -> bool {
    matches!(
        error,
        phoxal::bus::QueryError::Protocol(_)
            | phoxal::bus::QueryError::Decode(_)
            | phoxal::bus::QueryError::TooManyResponders
    ) || matches!(
        error,
        phoxal::bus::QueryError::Server(failure)
            if matches!(
                failure.code,
                phoxal::bus::QueryCode::InvalidArgument
                    | phoxal::bus::QueryCode::Internal
                    | phoxal::bus::QueryCode::NotFound
                    | phoxal::bus::QueryCode::Unimplemented
            )
    )
}

#[cfg(test)]
fn submap_has_drivable_space(response: &api::map::SubmapResponse) -> Result<bool> {
    let window = match response {
        api::map::SubmapResponse::Window(window) | api::map::SubmapResponse::Partial { window } => {
            window
        }
        api::map::SubmapResponse::OutOfBounds { .. } => {
            bail!("map query was out of bounds")
        }
    };
    let expected = usize::try_from(window.width)
        .ok()
        .and_then(|width| {
            usize::try_from(window.height)
                .ok()
                .and_then(|height| width.checked_mul(height))
        })
        .context("map dimensions overflow")?;
    if expected == 0 || window.cells.len() != expected {
        bail!(
            "map submap shape mismatch: {}x{} requires {expected} cells, got {}",
            window.width,
            window.height,
            window.cells.len()
        );
    }
    if !window.resolution_m.is_finite() || window.resolution_m <= 0.0 {
        bail!("map submap resolution must be finite and positive");
    }
    Ok(window
        .cells
        .iter()
        .any(|cell| matches!(cell, api::map::Occupancy::Free)))
}

fn constraint_reason(constraint: &api::safety::Constraint) -> api::safety::ConstraintReason {
    match constraint {
        api::safety::Constraint::Limited { reason, .. }
        | api::safety::Constraint::Stopped { reason, .. } => reason.clone(),
    }
}

fn stop_constraint(
    reason: api::safety::ConstraintReason,
    source: api::safety::ConstraintSource,
    observed_value: Option<f32>,
    now: RobotInstant,
    expires_at: RobotInstant,
) -> api::safety::Constraint {
    api::safety::Constraint::Stopped {
        reason,
        source,
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
    api::safety::Constraint::Limited {
        reason,
        source,
        max_linear_speed_mps,
        max_angular_speed_radps: f32::MAX,
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
        api::safety::ConstraintSourceKind::Footprint => WORLD_MODEL_PARTICIPANT_ID,
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

    fn test_footprint() -> FootprintEnvelope {
        FootprintEnvelope::new(0.1, 0.0).expect("test footprint is finite")
    }

    fn test_window(revision: u64) -> api::map::GridWindow {
        let bounds = api::map::Bounds {
            min_x_m: 0.0,
            min_y_m: 0.0,
            max_x_m: 3.2,
            max_y_m: 3.2,
        };
        api::map::GridWindow {
            frame_id: MAP_FRAME.to_string(),
            origin_pose: api::map::Pose {
                x_m: 0.0,
                y_m: 0.0,
                yaw_rad: 0.0,
            },
            cell_origin: api::map::Point { x_m: 0.0, y_m: 0.0 },
            resolution_m: 0.05,
            width: 64,
            height: 64,
            cells: vec![api::map::Occupancy::Free; 64 * 64],
            revision,
            requested: bounds.clone(),
            covered: bounds,
        }
    }

    fn is_stopped(motion: &api::safety::MotionConstraints) -> bool {
        matches!(
            motion.permission,
            api::safety::MotionPermission::Stopped { .. }
        )
    }

    fn constraint_reason(constraint: &api::safety::Constraint) -> api::safety::ConstraintReason {
        super::constraint_reason(constraint)
    }

    fn constraint_source(constraint: &api::safety::Constraint) -> &api::safety::ConstraintSource {
        match constraint {
            api::safety::Constraint::Limited { source, .. }
            | api::safety::Constraint::Stopped { source, .. } => source,
        }
    }

    fn constraint_observed_value(constraint: &api::safety::Constraint) -> Option<f32> {
        match constraint {
            api::safety::Constraint::Limited { observed_value, .. }
            | api::safety::Constraint::Stopped { observed_value, .. } => *observed_value,
        }
    }

    fn nominal_world() -> WorldInputs {
        let at = now();
        let mut world = WorldInputs::new([range_ref()], [capability("pack.battery")]);
        world.localization = Some(Timed::new(
            api::localize::LocalizationState {
                x_m: 1.6,
                y_m: 1.6,
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
        world.map_window = Some(Timed::new(test_window(1), at));
        world
            .batteries
            .insert(capability("pack.battery"), Some(battery_at(0.8, at)));
        world.map_health = MapHealth {
            healthy: true,
            stale: false,
            partial: false,
            terminal: false,
            detail: None,
        };
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
        battery_at(charge_ratio, now())
    }

    fn battery_at(
        charge_ratio: f32,
        observed_at: RobotInstant,
    ) -> Timed<api::component::battery::State> {
        Timed::new(
            api::component::battery::State {
                voltage_v: 16.0,
                current_a: 2.0,
                charge_ratio,
            },
            observed_at,
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

        let result = world.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(is_stopped(&result));
        let constraint = result
            .constraints
            .iter()
            .find(|constraint| {
                constraint_reason(constraint) == api::safety::ConstraintReason::BatteryCritical
            })
            .expect("the flat pack must raise a critical-battery stop");
        assert_eq!(constraint_observed_value(constraint), Some(0.04));
    }

    #[test]
    fn a_stale_pack_is_ignored_rather_than_trusted() {
        let mut world = nominal_world();
        let mut stale = battery(0.04);
        stale.at = RobotInstant::new(line(3), 0);
        world.batteries = BTreeMap::from([(capability("pack.battery"), Some(stale))]);

        let result = world.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(
            !result.constraints.iter().any(|constraint| {
                constraint_reason(constraint) == api::safety::ConstraintReason::BatteryCritical
            }),
            "a pack that stopped reporting cannot keep asserting a charge level"
        );
    }

    #[test]
    fn nominal_world_is_clear_and_expires_after_three_periods() {
        let result = nominal_world()
            .assess(7, now(), Some(test_footprint()))
            .unwrap();
        assert!(!is_stopped(&result));
        assert!(result.constraints.is_empty());
        assert_eq!(result.sequence, 7);
        assert_eq!(
            result.expires_at.duration_since(now()).unwrap(),
            CONSTRAINT_TTL
        );
    }

    #[test]
    fn a_valid_persisted_clearance_is_part_of_the_checked_footprint() {
        let footprint = FootprintEnvelope::new(0.05, 0.10).expect("finite clearance");
        let result = nominal_world().assess(8, now(), Some(footprint)).unwrap();
        assert!(matches!(
            result.permission,
            api::safety::MotionPermission::Clear
        ));
    }

    #[test]
    fn missing_world_inputs_fail_closed_with_typed_reasons() {
        let world = WorldInputs::new([range_ref()], [capability("pack.battery")]);
        let result = world.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(is_stopped(&result));
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::LocalizationUnavailable
        }));
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::MapUnavailable
        }));
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::WorldUnavailable
                && constraint_source(constraint).component_id.as_deref() == Some("front")
        }));
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::BatteryUnavailable
                && constraint_source(constraint).component_id.as_deref() == Some("pack")
        }));
    }

    #[test]
    fn a_declared_stale_battery_fails_closed_per_reference() {
        let at = now();
        let mut world = nominal_world();
        world.batteries.insert(
            capability("pack.battery"),
            Some(Timed::new(
                api::component::battery::State {
                    voltage_v: 16.0,
                    current_a: 0.0,
                    charge_ratio: 1.0,
                },
                RobotInstant::new(line(4), at.ticks()),
            )),
        );
        let result = world.assess(1, at, Some(test_footprint())).unwrap();
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::BatteryStale
                && constraint_source(constraint).component_id.as_deref() == Some("pack")
        }));
        assert!(is_stopped(&result));
    }

    #[test]
    fn queued_map_snapshot_does_not_rejuvenate_after_a_pause() {
        let completed_at = LocalInstant::from_boot_ns(10);
        let resumed_at = completed_at.saturating_add(MAP_STALE + Duration::from_nanos(1));
        assert!(!map_snapshot_is_fresh(completed_at, resumed_at));
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
        let result = world.assess(1, now(), Some(test_footprint())).unwrap();
        let reported: Vec<_> = result
            .constraints
            .iter()
            .filter(|constraint| {
                constraint_source(constraint).kind == api::safety::ConstraintSourceKind::Range
            })
            .map(|constraint| {
                (
                    constraint_source(constraint).component_id.clone(),
                    constraint_source(constraint).capability_id.clone(),
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
        let stopped = world.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(is_stopped(&stopped));
        assert_eq!(
            constraint_reason(&stopped.constraints[0]),
            api::safety::ConstraintReason::ObstacleProximity
        );
        assert_eq!(
            constraint_source(&stopped.constraints[0])
                .component_id
                .as_deref(),
            Some("front")
        );

        set_range_distance(&mut world, 0.5);
        let limited = world.assess(2, now(), Some(test_footprint())).unwrap();
        let api::safety::MotionPermission::Limited {
            effective_linear_speed_mps,
            ..
        } = limited.permission
        else {
            panic!("proximity should produce a limited permission");
        };
        assert_eq!(effective_linear_speed_mps, PROXIMITY_LINEAR_LIMIT_MPS);
    }

    #[test]
    fn samples_from_a_retired_world_never_authorize_motion() {
        let mut world = nominal_world();
        world.localization.as_mut().unwrap().at = RobotInstant::new(line(2), now().ticks());
        let result = world.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(is_stopped(&result));
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::LocalizationUnavailable
        }));
    }

    /// A world replacement retires every sample while keeping the declared
    /// capabilities, so each one must publish again before it counts.
    #[test]
    fn clearing_retires_every_sample_and_keeps_the_declared_capabilities() {
        let mut world = nominal_world();
        world.clear();
        let result = world.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(is_stopped(&result));
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::WorldUnavailable
                && constraint_source(constraint).component_id.as_deref() == Some("front")
        }));
    }

    #[test]
    fn submap_content_is_validated_and_requires_known_free_space() {
        let mut response_window = test_window(1);
        response_window.width = 2;
        response_window.height = 1;
        response_window.cells = vec![api::map::Occupancy::Unknown, api::map::Occupancy::Free];
        response_window.requested.max_x_m = 0.1;
        response_window.requested.max_y_m = 0.05;
        response_window.covered = response_window.requested.clone();
        let response = api::map::SubmapResponse::Window(response_window.clone());
        assert!(submap_has_drivable_space(&response).unwrap());
        response_window.cells = vec![api::map::Occupancy::Unknown, api::map::Occupancy::Occupied];
        assert!(
            !submap_has_drivable_space(&api::map::SubmapResponse::Window(response_window.clone(),))
                .unwrap()
        );
        response_window.cells = vec![api::map::Occupancy::Free];
        assert!(
            submap_has_drivable_space(&api::map::SubmapResponse::Window(response_window)).is_err()
        );
    }

    #[test]
    fn whole_compiled_footprint_must_be_free() {
        let mut world = nominal_world();
        let window = world.map_window.as_mut().unwrap();
        // The localization pose is at the lower-left corner of this cell. An
        // obstacle in that cell is inside the footprint even though other
        // cells in the radial envelope remain free.
        window.body.cells[32 * 64 + 32] = api::map::Occupancy::Occupied;
        let result = world.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::FootprintObstacle
        }));
    }

    #[test]
    fn unknown_cells_inside_the_footprint_fail_closed() {
        let mut world = nominal_world();
        world.map_window.as_mut().unwrap().body.cells[32 * 64 + 32] = api::map::Occupancy::Unknown;
        let result = world.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::UnknownOccupancy
        }));
    }

    #[test]
    fn stale_or_out_of_bounds_windows_fail_closed() {
        let mut stale = nominal_world();
        stale.map_window.as_mut().unwrap().at =
            RobotInstant::new(line(3), now().ticks().saturating_sub(1_000_000_000));
        let stale_result = stale.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(stale_result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::MapStale
        }));

        let mut out_of_bounds = nominal_world();
        out_of_bounds.localization.as_mut().unwrap().body.x_m = 0.02;
        let out_of_bounds_result = out_of_bounds
            .assess(2, now(), Some(test_footprint()))
            .unwrap();
        assert!(out_of_bounds_result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::FootprintMismatch
        }));
    }

    #[test]
    fn terminal_map_query_evidence_remains_a_stop() {
        let mut world = nominal_world();
        world.map_health.healthy = false;
        world.map_health.terminal = true;
        world.map_health.detail = Some("decode failure".to_string());
        let result = world.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::MapUnavailable
        }));
    }

    #[test]
    fn partial_map_query_evidence_remains_a_stop() {
        let mut world = nominal_world();
        world.map_health.healthy = false;
        world.map_health.partial = true;
        let result = world.assess(1, now(), Some(test_footprint())).unwrap();
        assert!(result.constraints.iter().any(|constraint| {
            constraint_reason(constraint) == api::safety::ConstraintReason::MapPartial
        }));
    }

    #[test]
    fn terminal_query_error_classification_is_explicit() {
        assert!(map_query_is_terminal(&phoxal::bus::QueryError::Decode(
            "bad grid".to_string(),
        )));
        assert!(map_query_is_terminal(&phoxal::bus::QueryError::Protocol(
            "bad frame".to_string(),
        )));
        assert!(map_query_is_terminal(
            &phoxal::bus::QueryError::TooManyResponders
        ));
        assert!(!map_query_is_terminal(
            &phoxal::bus::QueryError::Unavailable
        ));
        assert!(map_query_is_terminal(&phoxal::bus::QueryError::Server(
            phoxal::bus::QueryFailure::internal("map crashed"),
        )));
        assert!(!map_query_is_terminal(&phoxal::bus::QueryError::Server(
            phoxal::bus::QueryFailure::unavailable("map is not ready"),
        )));
    }
}
