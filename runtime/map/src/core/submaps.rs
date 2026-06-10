use std::collections::BTreeMap;

use phoxal::api::v1::frame::FrameId;
use phoxal::api::v1::localize::{Keyframe, KeyframeId, LocalizationRevisionId};
use phoxal::api::v1::map::{Submap, SubmapId};

#[derive(Debug, Clone, PartialEq)]
pub struct SubmapMetadata {
    pub submap_id: SubmapId,
    pub keyframe_id: KeyframeId,
    pub built_from_localize_revision: LocalizationRevisionId,
    pub anchor_frame_id: FrameId,
    pub anchor_translation_m: [f64; 3],
    pub anchor_rotation_xyzw: [f64; 4],
}

#[derive(Debug, Clone, Default)]
pub struct SubmapStore {
    by_id: BTreeMap<SubmapId, SubmapMetadata>,
    insertion_order: Vec<SubmapId>,
}

impl SubmapStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Ingest a keyframe. Returns Some(metadata) if a new submap was created;
    /// None if the keyframe id was already known.
    pub fn ingest(&mut self, keyframe: &Keyframe) -> Option<SubmapMetadata> {
        let submap_id = SubmapId::new(format!("submap-{}", keyframe.keyframe_id.0));
        if self.by_id.contains_key(&submap_id) {
            return None;
        }

        let metadata = SubmapMetadata {
            submap_id: submap_id.clone(),
            keyframe_id: keyframe.keyframe_id.clone(),
            built_from_localize_revision: keyframe.revision,
            anchor_frame_id: keyframe.pose.frame_id.clone(),
            anchor_translation_m: keyframe.pose.translation_m,
            anchor_rotation_xyzw: keyframe.pose.rotation_xyzw,
        };
        self.insertion_order.push(submap_id.clone());
        self.by_id.insert(submap_id, metadata.clone());
        Some(metadata)
    }

    /// Returns the most recently created submap, if any. Used by SubmapResponse.
    pub fn latest(&self) -> Option<&SubmapMetadata> {
        self.insertion_order
            .last()
            .and_then(|submap_id| self.by_id.get(submap_id))
    }

    /// Returns all known submaps in insertion order. Used by SnapshotResponse.
    pub fn all(&self) -> Vec<&SubmapMetadata> {
        self.insertion_order
            .iter()
            .filter_map(|submap_id| self.by_id.get(submap_id))
            .collect()
    }

    /// Count of stored submaps.
    pub fn len(&self) -> usize {
        self.by_id.len()
    }
}

impl SubmapMetadata {
    /// Build the wire-shape Submap with empty bytes (sensor integration is P2.3.B.2).
    pub fn to_empty_submap(&self) -> Submap {
        Submap {
            submap_id: self.submap_id.clone(),
            bytes: Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use phoxal::api::v1::localize::PoseEstimate;

    use super::*;

    #[test]
    fn first_keyframe_seeds_submap() {
        let mut store = SubmapStore::new();

        let created = store.ingest(&keyframe("kf-a", 1, [1.0, 2.0, 3.0], [0.0, 0.0, 0.0, 1.0]));

        assert_eq!(store.len(), 1);
        assert_eq!(
            created.map(|metadata| metadata.submap_id),
            Some(SubmapId::new("submap-kf-a"))
        );
    }

    #[test]
    fn duplicate_keyframe_is_idempotent() {
        let mut store = SubmapStore::new();
        let keyframe = keyframe("kf-a", 1, [1.0, 2.0, 3.0], [0.0, 0.0, 0.0, 1.0]);

        assert!(store.ingest(&keyframe).is_some());
        assert_eq!(store.ingest(&keyframe), None);

        assert_eq!(store.len(), 1);
    }

    #[test]
    fn distinct_keyframes_create_distinct_submaps() {
        let mut store = SubmapStore::new();

        let first = store.ingest(&keyframe("kf-a", 1, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
        let second = store.ingest(&keyframe("kf-b", 1, [1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
        let third = store.ingest(&keyframe("kf-c", 1, [2.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));

        assert_eq!(store.len(), 3);
        assert_eq!(
            [first, second, third].map(|created| created.map(|metadata| metadata.submap_id)),
            [
                Some(SubmapId::new("submap-kf-a")),
                Some(SubmapId::new("submap-kf-b")),
                Some(SubmapId::new("submap-kf-c")),
            ]
        );
    }

    #[test]
    fn latest_returns_most_recently_ingested() {
        let mut store = SubmapStore::new();

        let _ = store.ingest(&keyframe("kf-a", 1, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
        let _ = store.ingest(&keyframe("kf-b", 1, [1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
        let _ = store.ingest(&keyframe("kf-c", 1, [2.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));

        assert_eq!(
            store.latest().map(|metadata| metadata.submap_id.clone()),
            Some(SubmapId::new("submap-kf-c"))
        );
    }

    #[test]
    fn all_returns_insertion_order() {
        let mut store = SubmapStore::new();

        let _ = store.ingest(&keyframe("kf-b", 1, [1.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
        let _ = store.ingest(&keyframe("kf-a", 1, [0.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));
        let _ = store.ingest(&keyframe("kf-c", 1, [2.0, 0.0, 0.0], [0.0, 0.0, 0.0, 1.0]));

        assert_eq!(
            store
                .all()
                .iter()
                .map(|metadata| metadata.submap_id.clone())
                .collect::<Vec<_>>(),
            vec![
                SubmapId::new("submap-kf-b"),
                SubmapId::new("submap-kf-a"),
                SubmapId::new("submap-kf-c"),
            ]
        );
    }

    #[test]
    fn metadata_carries_keyframe_pose_anchor() {
        let mut store = SubmapStore::new();
        let keyframe = keyframe("kf-a", 7, [1.25, -2.5, 0.75], [0.1, 0.2, 0.3, 0.9]);

        let created = store.ingest(&keyframe);

        assert_eq!(
            created.map(|metadata| {
                (
                    metadata.anchor_frame_id,
                    metadata.anchor_translation_m,
                    metadata.anchor_rotation_xyzw,
                )
            }),
            Some((
                FrameId::new("map"),
                [1.25, -2.5, 0.75],
                [0.1, 0.2, 0.3, 0.9],
            ))
        );
    }

    fn keyframe(
        keyframe_id: &str,
        sequence: u64,
        translation_m: [f64; 3],
        rotation_xyzw: [f64; 4],
    ) -> Keyframe {
        Keyframe {
            keyframe_id: KeyframeId::new(keyframe_id),
            revision: LocalizationRevisionId { epoch: 1, sequence },
            pose: PoseEstimate {
                frame_id: FrameId::new("map"),
                child_frame_id: FrameId::new("base_link"),
                translation_m,
                rotation_xyzw,
            },
            descriptors: Vec::new(),
        }
    }
}
