//! Detector heads: the `DetectorHead` abstraction, the deterministic
//! placeholder that backs it by default, and the conversion from a head's raw
//! output into the published `Detection` body.
//!
//! `DetectorHead` is the seam that keeps a model backend out of the participant
//! IO surface: a head sees one camera frame plus optional depth and
//! localization, and answers with positions in the sensor frame. Nothing about
//! the topics, the tracker, or the health reporting depends on which head is
//! installed.

use phoxal::api;

/// Everything a head is given about one frame.
///
/// This is the head contract, so the fields are populated whether or not the
/// installed head consults them. Only [`PlaceholderDetector`] is linked and it
/// reads none of them, which is what the `dead_code` expectation records; a head
/// that reads any field retires the expectation.
#[expect(
    dead_code,
    reason = "the head contract is populated for every head; the only linked head reads none of it"
)]
pub(crate) struct DetectorInput<'a> {
    pub(crate) camera: &'a api::component::camera::Frame,
    pub(crate) depth: Option<&'a api::component::depth::Frame>,
    pub(crate) frame_id: &'a str,
    pub(crate) stamp_ns: u64,
    pub(crate) localization: Option<&'a api::localize::LocalizationState>,
}

/// One detection as a head reports it: a position in the sensor frame, before
/// any frame transform or track association.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RawDetection {
    pub(crate) class_id: String,
    pub(crate) confidence: f32,
    pub(crate) position_m: [f64; 3],
}

impl RawDetection {
    /// Lift a raw detection into the published body.
    ///
    /// With a usable localization the position is rotated and translated into
    /// the map frame, and the detection says so; without one it stays in the
    /// sensor frame it was measured in. The reported `frame_id` always names
    /// the frame the position is actually expressed in, so a consumer never has
    /// to guess which of the two it received.
    pub(crate) fn into_detection(
        self,
        source_frame_id: &str,
        localization: Option<&api::localize::LocalizationState>,
    ) -> api::perception::Detection {
        let (position_m, frame_id) = match localization {
            Some(localization) => (
                local_to_map_position(self.position_m, localization),
                "map".to_string(),
            ),
            None => (self.position_m, source_frame_id.to_string()),
        };
        api::perception::Detection {
            class_id: self.class_id,
            confidence: self.confidence,
            position_m,
            frame_id,
            track_id: None,
        }
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

pub(crate) trait DetectorHead {
    fn detector_name(&self) -> &'static str;
    fn detect(&mut self, input: DetectorInput<'_>) -> Vec<RawDetection>;
}

/// The default head. No model backend is linked, so it reports nothing rather
/// than inventing detections; the participant still publishes camera health.
pub(crate) struct PlaceholderDetector;

impl DetectorHead for PlaceholderDetector {
    fn detector_name(&self) -> &'static str {
        "deterministic-placeholder"
    }

    fn detect(&mut self, _input: DetectorInput<'_>) -> Vec<RawDetection> {
        Vec::new()
    }
}

#[cfg(test)]
mod tests {
    use super::{DetectorHead, DetectorInput, PlaceholderDetector, RawDetection};
    use phoxal::api;

    fn raw(position_m: [f64; 3]) -> RawDetection {
        RawDetection {
            class_id: "crate".to_string(),
            confidence: 0.9,
            position_m,
        }
    }

    #[test]
    fn placeholder_detector_emits_no_detections() {
        let mut detector = PlaceholderDetector;
        let camera = api::component::camera::Frame {
            width: 2,
            height: 2,
            encoding: api::component::camera::Encoding::Rgb8,
            intrinsics: None,
            distortion: None,
            exposure: None,
            calibration: None,
            data: vec![0; 12],
        };

        let detections = detector.detect(DetectorInput {
            camera: &camera,
            depth: None,
            frame_id: "front_camera__rgb_link",
            stamp_ns: 123,
            localization: None,
        });

        assert!(detections.is_empty());
        assert_eq!(detector.detector_name(), "deterministic-placeholder");
    }

    #[test]
    fn detection_uses_map_frame_when_localization_is_available() {
        let localization = api::localize::LocalizationState {
            x_m: 2.0,
            y_m: 3.0,
            yaw_rad: std::f64::consts::FRAC_PI_2,
            confidence: 1.0,
        };

        let detection = raw([1.0, 0.0, 0.5]).into_detection("camera", Some(&localization));

        assert_eq!(detection.frame_id, "map");
        assert!((detection.position_m[0] - 2.0).abs() < 1e-9);
        assert!((detection.position_m[1] - 4.0).abs() < 1e-9);
        assert_eq!(detection.position_m[2], 0.5);
    }

    #[test]
    fn detection_stays_in_the_sensor_frame_without_localization() {
        let detection = raw([1.0, -2.0, 0.5]).into_detection("front_camera__rgb_link", None);

        assert_eq!(detection.frame_id, "front_camera__rgb_link");
        assert_eq!(detection.position_m, [1.0, -2.0, 0.5]);
        assert_eq!(detection.track_id, None);
    }
}
