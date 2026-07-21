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
                let distance_m = distance_m(track.position_m, detection.position_m);
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

fn distance_m(left: [f64; 3], right: [f64; 3]) -> f64 {
    let dx = left[0] - right[0];
    let dy = left[1] - right[1];
    let dz = left[2] - right[2];
    (dx * dx + dy * dy + dz * dz).sqrt()
}
