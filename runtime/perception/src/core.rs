use phoxal::api::v1::perception::{
    BoundingBox, Detection, DetectorHealth, PerceptionDegradedReason, PerceptionStoppedReason,
    RevisionLinkage, TrackedObservation,
};

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TrackerConfig {
    pub association_window_ns: u64,
    pub association_max_center_distance_px: f32,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            association_window_ns: 500_000_000,
            association_max_center_distance_px: 32.0,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct TrackerUpdate {
    pub detections: Vec<Detection>,
    pub observations: Vec<TrackedObservation>,
}

#[derive(Debug, Clone)]
struct Track {
    observation: TrackedObservation,
    class_id: u32,
    bbox: BoundingBox,
}

#[derive(Debug, Clone)]
pub struct Tracker {
    config: TrackerConfig,
    next_tracker_id: u64,
    tracks: Vec<Track>,
    active_revision: Option<RevisionLinkage>,
}

impl Tracker {
    #[must_use]
    pub fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            next_tracker_id: 1,
            tracks: Vec::new(),
            active_revision: None,
        }
    }

    #[must_use]
    pub fn update(
        &mut self,
        detections: Vec<Detection>,
        revision: RevisionLinkage,
        observed_at_ns: u64,
    ) -> TrackerUpdate {
        let revision_changed = self
            .active_revision
            .is_some_and(|active_revision| active_revision != revision);
        if revision_changed {
            for track in &mut self.tracks {
                track.observation.localize_revision = revision.localize_revision;
                track.observation.map_revision = revision.map_revision;
                track.observation.anchor_3d_m = None;
            }
        }
        self.active_revision = Some(revision);
        self.prune_expired(observed_at_ns);

        let mut assigned_track_indices = Vec::new();
        let mut tracked_detections = Vec::with_capacity(detections.len());
        for mut detection in detections {
            if let Some(track_index) =
                self.best_track_for(&detection, observed_at_ns, &assigned_track_indices)
            {
                let track = &mut self.tracks[track_index];
                track.bbox = detection.bbox;
                track.class_id = detection.class_id;
                track.observation.last_seen_ns = observed_at_ns;
                track.observation.localize_revision = revision.localize_revision;
                track.observation.map_revision = revision.map_revision;
                track.observation.anchor_3d_m = detection.anchor_3d_m;
                track.observation.source_frame_id = detection.source_frame_id.clone();
                detection.tracker_id = Some(track.observation.tracker_id);
                assigned_track_indices.push(track_index);
            } else {
                let tracker_id = self.next_tracker_id;
                self.next_tracker_id += 1;
                let observation = TrackedObservation {
                    tracker_id,
                    last_seen_ns: observed_at_ns,
                    localize_revision: revision.localize_revision,
                    map_revision: revision.map_revision,
                    anchor_3d_m: detection.anchor_3d_m,
                    source_frame_id: detection.source_frame_id.clone(),
                };
                self.tracks.push(Track {
                    observation,
                    class_id: detection.class_id,
                    bbox: detection.bbox,
                });
                detection.tracker_id = Some(tracker_id);
                assigned_track_indices.push(self.tracks.len() - 1);
            }
            tracked_detections.push(detection);
        }

        TrackerUpdate {
            detections: tracked_detections,
            observations: self
                .tracks
                .iter()
                .map(|track| track.observation.clone())
                .collect(),
        }
    }

    fn prune_expired(&mut self, observed_at_ns: u64) {
        let window_ns = self.config.association_window_ns;
        self.tracks.retain(|track| {
            observed_at_ns.saturating_sub(track.observation.last_seen_ns) <= window_ns
        });
    }

    fn best_track_for(
        &self,
        detection: &Detection,
        observed_at_ns: u64,
        assigned_track_indices: &[usize],
    ) -> Option<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(index, track)| {
                !assigned_track_indices.contains(index)
                    && track.class_id == detection.class_id
                    && observed_at_ns.saturating_sub(track.observation.last_seen_ns)
                        <= self.config.association_window_ns
            })
            .filter_map(|(index, track)| {
                let distance_px = bbox_center_distance_px(track.bbox, detection.bbox);
                (distance_px <= self.config.association_max_center_distance_px)
                    .then_some((index, distance_px))
            })
            .min_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(index, _)| index)
    }
}

impl Default for Tracker {
    fn default() -> Self {
        Self::new(TrackerConfig::default())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HealthState {
    health: DetectorHealth,
}

impl HealthState {
    #[must_use]
    pub const fn new() -> Self {
        Self {
            health: DetectorHealth::Healthy,
        }
    }

    #[must_use]
    pub const fn health(self) -> DetectorHealth {
        self.health
    }

    pub const fn observe_healthy(&mut self) {
        self.health = DetectorHealth::Healthy;
    }

    pub const fn degrade(&mut self, reason: PerceptionDegradedReason) {
        self.health = DetectorHealth::Degraded(reason);
    }

    pub const fn stop(&mut self, reason: PerceptionStoppedReason) {
        self.health = DetectorHealth::Stopped(reason);
    }
}

impl Default for HealthState {
    fn default() -> Self {
        Self::new()
    }
}

fn bbox_center_distance_px(left: BoundingBox, right: BoundingBox) -> f32 {
    let left_center_x = left.x + left.width * 0.5;
    let left_center_y = left.y + left.height * 0.5;
    let right_center_x = right.x + right.width * 0.5;
    let right_center_y = right.y + right.height * 0.5;
    let dx = left_center_x - right_center_x;
    let dy = left_center_y - right_center_y;
    (dx * dx + dy * dy).sqrt()
}

#[cfg(test)]
mod tests {
    use phoxal::api::v1::frame::FrameId;
    use phoxal::api::v1::localize::LocalizationRevisionId;
    use phoxal::api::v1::map::MapRevisionId;

    use super::*;

    #[test]
    fn dedups_same_object_within_window_and_separates_distant_detection() {
        let mut tracker = Tracker::new(TrackerConfig {
            association_window_ns: 1_000,
            association_max_center_distance_px: 8.0,
        });
        let revision = revision(1, 1);

        let first = tracker.update(vec![detection(10.0, Some([1.0, 0.0, 0.0]))], revision, 100);
        let first_id = first.detections[0].tracker_id.expect("tracker id");
        let second = tracker.update(vec![detection(13.0, Some([1.1, 0.0, 0.0]))], revision, 200);
        let second_id = second.detections[0].tracker_id.expect("tracker id");
        let distant = tracker.update(vec![detection(200.0, Some([5.0, 0.0, 0.0]))], revision, 300);
        let distant_id = distant.detections[0].tracker_id.expect("tracker id");

        assert_eq!(first_id, second_id);
        assert_ne!(second_id, distant_id);
        assert_eq!(second.observations.len(), 1);
        assert_eq!(distant.observations.len(), 2);
    }

    #[test]
    fn revision_change_invalidates_stale_anchor_and_reemits_survivor() {
        let mut tracker = Tracker::new(TrackerConfig::default());
        let old_revision = revision(1, 1);
        let new_revision = revision(2, 1);

        let first = tracker.update(
            vec![detection(10.0, Some([1.0, 0.0, 0.0]))],
            old_revision,
            100,
        );
        let tracker_id = first.detections[0].tracker_id.expect("tracker id");
        let invalidated = tracker.update(Vec::new(), new_revision, 200);

        assert_eq!(invalidated.observations.len(), 1);
        assert_eq!(invalidated.observations[0].tracker_id, tracker_id);
        assert_eq!(
            invalidated.observations[0].localize_revision,
            new_revision.localize_revision
        );
        assert_eq!(
            invalidated.observations[0].map_revision,
            new_revision.map_revision
        );
        assert_eq!(invalidated.observations[0].anchor_3d_m, None);

        let reanchored = tracker.update(
            vec![detection(11.0, Some([1.2, 0.0, 0.0]))],
            new_revision,
            300,
        );

        assert_eq!(reanchored.detections[0].tracker_id, Some(tracker_id));
        assert_eq!(
            reanchored.observations[0].anchor_3d_m,
            Some([1.2, 0.0, 0.0])
        );
        assert_eq!(
            reanchored.observations[0].localize_revision,
            new_revision.localize_revision
        );
    }

    #[test]
    fn health_transitions_are_typed() {
        let mut state = HealthState::new();

        assert_eq!(state.health(), DetectorHealth::Healthy);
        state.degrade(PerceptionDegradedReason::SourceStale);
        assert_eq!(
            state.health(),
            DetectorHealth::Degraded(PerceptionDegradedReason::SourceStale)
        );
        state.stop(PerceptionStoppedReason::ComputeUnavailable);
        assert_eq!(
            state.health(),
            DetectorHealth::Stopped(PerceptionStoppedReason::ComputeUnavailable)
        );
        state.observe_healthy();
        assert_eq!(state.health(), DetectorHealth::Healthy);
    }

    fn detection(x: f32, anchor_3d_m: Option<[f64; 3]>) -> Detection {
        Detection {
            class_label: "crate".to_string(),
            class_id: 7,
            confidence: 0.9,
            bbox: BoundingBox {
                x,
                y: 20.0,
                width: 20.0,
                height: 20.0,
            },
            anchor_3d_m,
            source_frame_id: FrameId::new("front_camera__camera_link"),
            tracker_id: None,
        }
    }

    const fn revision(epoch: u64, sequence: u64) -> RevisionLinkage {
        RevisionLinkage {
            localize_revision: LocalizationRevisionId { epoch, sequence },
            map_revision: MapRevisionId { epoch, sequence },
        }
    }
}
