//! Frame-sample bookkeeping: the timestamped sample wrapper, camera health
//! tracking, freshest-sample selection across sensors, and the raw-detection
//! to wire-`Detection` conversion (including the local-to-map frame
//! transform when localization is fresh and confident).

use phoxal::bus::LogicalTime;
use phoxal_api::v1 as api;

use crate::detector::RawDetection;
use crate::sensors::SensorBinding;

#[derive(Clone)]
pub(crate) struct FrameSample<B> {
    pub(crate) body: B,
    pub(crate) at: LogicalTime,
}

#[derive(Default)]
pub(crate) struct HealthState {
    healthy: bool,
}

impl HealthState {
    pub(crate) fn observe_healthy(&mut self) {
        self.healthy = true;
    }

    pub(crate) fn degrade(&mut self) {
        self.healthy = false;
    }

    pub(crate) fn healthy(&self) -> bool {
        self.healthy
    }
}

pub(crate) fn latest_fresh_camera_index<B>(
    samples: &[Option<FrameSample<B>>],
    now: LogicalTime,
    stale_ns: u64,
) -> Option<usize> {
    samples
        .iter()
        .enumerate()
        .filter_map(|(index, sample)| sample.as_ref().map(|sample| (index, sample)))
        .filter(|(_, sample)| sample_is_fresh(sample, now, stale_ns))
        .max_by_key(|(_, sample)| sample.at.time_ns())
        .map(|(index, _)| index)
}

pub(crate) fn latest_matching_depth(
    depth_sources: &[SensorBinding],
    samples: &[Option<FrameSample<api::component::depth::Frame>>],
    component_id: &str,
    now: LogicalTime,
    depth_stale_ns: u64,
) -> Option<FrameSample<api::component::depth::Frame>> {
    depth_sources
        .iter()
        .zip(samples)
        .filter(|(source, _)| source.component_id == component_id)
        .filter_map(|(_, sample)| sample.as_ref())
        .filter(|sample| sample_is_fresh(sample, now, depth_stale_ns))
        .max_by_key(|sample| sample.at.time_ns())
        .cloned()
}

pub(crate) fn fresh_localization(
    sample: &Option<FrameSample<api::localize::LocalizationState>>,
    now: LogicalTime,
    localization_stale_ns: u64,
    min_localization_confidence: f32,
) -> Option<&api::localize::LocalizationState> {
    let sample = sample.as_ref()?;
    if !sample_is_fresh(sample, now, localization_stale_ns) {
        return None;
    }
    (sample.body.confidence >= min_localization_confidence).then_some(&sample.body)
}

fn sample_is_fresh<B>(sample: &FrameSample<B>, now: LogicalTime, stale_ns: u64) -> bool {
    sample.at.epoch() == now.epoch()
        && sample.at.time_ns() <= now.time_ns()
        && now.time_ns().saturating_sub(sample.at.time_ns()) <= stale_ns
}

pub(crate) fn detection_from_raw(
    raw: RawDetection,
    source_frame_id: &str,
    localization: Option<&api::localize::LocalizationState>,
) -> api::perception::Detection {
    let (position_m, frame_id) = match localization {
        Some(localization) => (
            local_to_map_position(raw.position_m, localization),
            "map".to_string(),
        ),
        None => (raw.position_m, source_frame_id.to_string()),
    };
    api::perception::Detection {
        class_id: raw.class_id,
        confidence: raw.confidence,
        position_m,
        frame_id,
        track_id: None,
    }
}

fn local_to_map_position(
    position_m: [f64; 3],
    localization: &api::localize::LocalizationState,
) -> [f64; 3] {
    let yaw_cos = localization.yaw_rad.cos();
    let yaw_sin = localization.yaw_rad.sin();
    [
        localization.x_m + yaw_cos * position_m[0] - yaw_sin * position_m[1],
        localization.y_m + yaw_sin * position_m[0] + yaw_cos * position_m[1],
        position_m[2],
    ]
}
