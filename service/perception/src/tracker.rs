//! Point tracker: assigns stable `track_id`s to detections by nearest
//! same-class association within a time/distance window, pruning tracks that
//! have not been seen within that window.

use phoxal::api;

#[derive(Clone, Copy)]
pub(crate) struct TrackerConfig {
    pub(crate) association_window_ns: u64,
    pub(crate) association_max_distance_m: f64,
}

impl Default for TrackerConfig {
    fn default() -> Self {
        Self {
            association_window_ns: 500_000_000,
            association_max_distance_m: 0.5,
        }
    }
}

#[derive(Clone)]
struct Track {
    track_id: u64,
    class_id: String,
    position_m: [f64; 3],
    last_seen_ns: u64,
}

impl Track {
    /// Distance from this track to a detection, in metres.
    ///
    /// Association is a full 3-D distance, not the planar one the rest of the
    /// robot uses for poses: detections carry a height, and two objects stacked
    /// above one another are separate things that must not share a track.
    fn distance_m_to(&self, position_m: [f64; 3]) -> f64 {
        let dx = self.position_m[0] - position_m[0];
        let dy = self.position_m[1] - position_m[1];
        let dz = self.position_m[2] - position_m[2];
        (dx * dx + dy * dy + dz * dz).sqrt()
    }
}

pub(crate) struct PointTracker {
    config: TrackerConfig,
    next_track_id: u64,
    tracks: Vec<Track>,
}

impl PointTracker {
    pub(crate) fn new(config: TrackerConfig) -> Self {
        Self {
            config,
            next_track_id: 1,
            tracks: Vec::new(),
        }
    }

    pub(crate) fn update(
        &mut self,
        detections: &mut [api::perception::Detection],
        observed_at_ns: u64,
    ) {
        self.prune_expired(observed_at_ns);

        let mut assigned_track_indices = Vec::new();
        for detection in detections {
            if let Some(track_index) =
                self.best_track_for(detection, observed_at_ns, &assigned_track_indices)
            {
                let track = &mut self.tracks[track_index];
                track.position_m = detection.position_m;
                track.class_id.clone_from(&detection.class_id);
                track.last_seen_ns = observed_at_ns;
                detection.track_id = Some(track.track_id);
                assigned_track_indices.push(track_index);
            } else {
                let track_id = self.next_track_id;
                self.next_track_id = self.next_track_id.saturating_add(1);
                self.tracks.push(Track {
                    track_id,
                    class_id: detection.class_id.clone(),
                    position_m: detection.position_m,
                    last_seen_ns: observed_at_ns,
                });
                detection.track_id = Some(track_id);
                assigned_track_indices.push(self.tracks.len() - 1);
            }
        }
    }

    fn prune_expired(&mut self, observed_at_ns: u64) {
        let window_ns = self.config.association_window_ns;
        self.tracks
            .retain(|track| observed_at_ns.saturating_sub(track.last_seen_ns) <= window_ns);
    }

    fn best_track_for(
        &self,
        detection: &api::perception::Detection,
        observed_at_ns: u64,
        assigned_track_indices: &[usize],
    ) -> Option<usize> {
        self.tracks
            .iter()
            .enumerate()
            .filter(|(index, track)| {
                !assigned_track_indices.contains(index)
                    && track.class_id == detection.class_id
                    && observed_at_ns.saturating_sub(track.last_seen_ns)
                        <= self.config.association_window_ns
            })
            .filter_map(|(index, track)| {
                let distance_m = track.distance_m_to(detection.position_m);
                (distance_m <= self.config.association_max_distance_m)
                    .then_some((index, distance_m))
            })
            .min_by(|(_, left), (_, right)| left.total_cmp(right))
            .map(|(index, _)| index)
    }
}

impl Default for PointTracker {
    fn default() -> Self {
        Self::new(TrackerConfig::default())
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api;

    use super::{PointTracker, TrackerConfig};
    use crate::detector::RawDetection;

    fn detection(position_m: [f64; 3]) -> api::perception::Detection {
        RawDetection {
            class_id: "crate".to_string(),
            confidence: 0.9,
            position_m,
        }
        .into_detection("camera", None)
    }

    #[test]
    fn point_tracker_reuses_nearby_track_and_separates_distant_detection() {
        let mut tracker = PointTracker::new(TrackerConfig {
            association_window_ns: 1_000,
            association_max_distance_m: 0.5,
        });
        let mut first = vec![detection([1.0, 0.0, 0.0])];
        tracker.update(&mut first, 100);
        let first_id = first[0].track_id;

        let mut nearby = vec![detection([1.2, 0.0, 0.0])];
        tracker.update(&mut nearby, 200);
        assert_eq!(nearby[0].track_id, first_id);

        let mut distant = vec![detection([5.0, 0.0, 0.0])];
        tracker.update(&mut distant, 300);
        assert_ne!(distant[0].track_id, first_id);
    }

    /// Association is 3-D: a detection directly above a track is a different
    /// object, not the same one seen again.
    #[test]
    fn height_separates_two_otherwise_coincident_detections() {
        let mut tracker = PointTracker::new(TrackerConfig {
            association_window_ns: 1_000,
            association_max_distance_m: 0.5,
        });
        let mut ground = vec![detection([1.0, 0.0, 0.0])];
        tracker.update(&mut ground, 100);

        let mut above = vec![detection([1.0, 0.0, 1.0])];
        tracker.update(&mut above, 200);

        assert_ne!(above[0].track_id, ground[0].track_id);
    }
}
