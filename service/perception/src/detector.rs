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

use std::fmt;

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

/// Evidence that a detector output or the transform inputs cannot become a
/// valid published detection. This remains an internal typed error: the
/// public health state deliberately exposes the stable `DetectorFailure` or
/// `InvalidCamera` class, while logs/tests can retain the precise evidence.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DetectionValidationError {
    EmptyClassId,
    EmptyFrameId,
    NonFiniteConfidence,
    NonFinitePosition,
    NonFiniteLocalization,
    NonFiniteTransformedPosition,
}

impl fmt::Display for DetectionValidationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let message = match self {
            Self::EmptyClassId => "detector output has an empty class id",
            Self::EmptyFrameId => "detection has an empty frame id",
            Self::NonFiniteConfidence => "detector confidence is not finite",
            Self::NonFinitePosition => "detector position is not finite",
            Self::NonFiniteLocalization => "localization transform input is not finite",
            Self::NonFiniteTransformedPosition => "transformed detection position is not finite",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for DetectionValidationError {}

/// Failure classes a detector/backend can report without turning a cycle into
/// an implicit empty result. The placeholder currently never fails, but the
/// boundary is explicit for a model backend plugged into this participant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[expect(
    dead_code,
    reason = "the placeholder backend is infallible; these terminal classes are the detector seam"
)]
pub(crate) enum DetectorFailure {
    BackendUnavailable,
    Failed,
    InvalidOutput(DetectionValidationError),
}

impl RawDetection {
    /// Validate and lift a raw detection into the published body.
    ///
    /// With a usable localization the position is rotated and translated into
    /// the map frame, and the detection says so; without one it stays in the
    /// sensor frame it was measured in. The reported `frame_id` always names
    /// the frame the position is actually expressed in, so a consumer never has
    /// to guess which of the two it received.
    pub(crate) fn try_into_detection(
        self,
        source_frame_id: &str,
        localization: Option<&api::localize::LocalizationState>,
    ) -> Result<api::perception::Detection, DetectorFailure> {
        self.validate().map_err(DetectorFailure::InvalidOutput)?;
        let (position_m, frame_id) = match localization {
            Some(localization) => (
                local_to_map_position(self.position_m, localization)
                    .map_err(DetectorFailure::InvalidOutput)?,
                "map".to_string(),
            ),
            None => (self.position_m, source_frame_id.to_string()),
        };
        let detection = api::perception::Detection {
            class_id: self.class_id,
            confidence: self.confidence,
            position_m,
            frame_id,
            track_id: None,
        };
        validate_detection(&detection).map_err(DetectorFailure::InvalidOutput)?;
        Ok(detection)
    }

    fn validate(&self) -> Result<(), DetectionValidationError> {
        if self.class_id.is_empty() {
            return Err(DetectionValidationError::EmptyClassId);
        }
        if !self.confidence.is_finite() {
            return Err(DetectionValidationError::NonFiniteConfidence);
        }
        if !self.position_m.into_iter().all(f64::is_finite) {
            return Err(DetectionValidationError::NonFinitePosition);
        }
        Ok(())
    }
}

fn local_to_map_position(
    position_m: [f64; 3],
    localization: &api::localize::LocalizationState,
) -> Result<[f64; 3], DetectionValidationError> {
    if ![
        localization.x_m,
        localization.y_m,
        localization.yaw_rad,
        f64::from(localization.confidence),
    ]
    .into_iter()
    .all(f64::is_finite)
    {
        return Err(DetectionValidationError::NonFiniteLocalization);
    }
    let yaw_cos = localization.yaw_rad.cos();
    let yaw_sin = localization.yaw_rad.sin();
    let transformed = [
        localization.x_m + yaw_cos * position_m[0] - yaw_sin * position_m[1],
        localization.y_m + yaw_sin * position_m[0] + yaw_cos * position_m[1],
        position_m[2],
    ];
    transformed
        .into_iter()
        .all(f64::is_finite)
        .then_some(transformed)
        .ok_or(DetectionValidationError::NonFiniteTransformedPosition)
}

fn validate_detection(
    detection: &api::perception::Detection,
) -> Result<(), DetectionValidationError> {
    if detection.class_id.is_empty() {
        return Err(DetectionValidationError::EmptyClassId);
    }
    if detection.frame_id.is_empty() {
        return Err(DetectionValidationError::EmptyFrameId);
    }
    if !detection.confidence.is_finite() {
        return Err(DetectionValidationError::NonFiniteConfidence);
    }
    if !detection.position_m.into_iter().all(f64::is_finite) {
        return Err(DetectionValidationError::NonFinitePosition);
    }
    Ok(())
}

pub(crate) trait DetectorHead {
    fn detector_name(&self) -> &'static str;
    fn detect(&mut self, input: DetectorInput<'_>) -> Result<Vec<RawDetection>, DetectorFailure>;
}

/// The default head. No model backend is linked, so it explicitly reports that
/// no detection result is available.
pub(crate) struct PlaceholderDetector;

impl DetectorHead for PlaceholderDetector {
    fn detector_name(&self) -> &'static str {
        "deterministic-placeholder"
    }

    fn detect(&mut self, _input: DetectorInput<'_>) -> Result<Vec<RawDetection>, DetectorFailure> {
        Err(DetectorFailure::BackendUnavailable)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DetectionValidationError, DetectorFailure, DetectorHead, DetectorInput,
        PlaceholderDetector, RawDetection,
    };
    use phoxal::api;

    fn raw(position_m: [f64; 3]) -> RawDetection {
        RawDetection {
            class_id: "crate".to_string(),
            confidence: 0.9,
            position_m,
        }
    }

    #[test]
    fn placeholder_detector_reports_backend_unavailable() {
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

        let error = detector
            .detect(DetectorInput {
                camera: &camera,
                depth: None,
                frame_id: "front_camera__rgb_link",
                stamp_ns: 123,
                localization: None,
            })
            .unwrap_err();

        assert_eq!(error, DetectorFailure::BackendUnavailable);
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

        let detection = raw([1.0, 0.0, 0.5])
            .try_into_detection("camera", Some(&localization))
            .unwrap();

        assert_eq!(detection.frame_id, "map");
        assert!((detection.position_m[0] - 2.0).abs() < 1e-9);
        assert!((detection.position_m[1] - 4.0).abs() < 1e-9);
        assert_eq!(detection.position_m[2], 0.5);
    }

    #[test]
    fn detection_stays_in_the_sensor_frame_without_localization() {
        let detection = raw([1.0, -2.0, 0.5])
            .try_into_detection("front_camera__rgb_link", None)
            .unwrap();

        assert_eq!(detection.frame_id, "front_camera__rgb_link");
        assert_eq!(detection.position_m, [1.0, -2.0, 0.5]);
        assert_eq!(detection.track_id, None);
    }

    #[test]
    fn raw_nonfinite_fields_are_rejected_before_publication() {
        for confidence in [f32::NAN, f32::INFINITY, f32::NEG_INFINITY] {
            let error = RawDetection {
                confidence,
                ..raw([1.0, 2.0, 3.0])
            }
            .try_into_detection("camera", None)
            .unwrap_err();
            assert_eq!(
                error,
                DetectorFailure::InvalidOutput(DetectionValidationError::NonFiniteConfidence)
            );
        }

        for position_index in 0..3 {
            for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
                let mut position = [1.0, 2.0, 3.0];
                position[position_index] = value;
                let error = raw(position)
                    .try_into_detection("camera", None)
                    .unwrap_err();
                assert_eq!(
                    error,
                    DetectorFailure::InvalidOutput(DetectionValidationError::NonFinitePosition)
                );
            }
        }
    }

    #[test]
    fn nonfinite_localization_is_rejected_before_transform() {
        for value in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            let localization = api::localize::LocalizationState {
                x_m: value,
                y_m: 0.0,
                yaw_rad: 0.0,
                confidence: 1.0,
            };
            let error = raw([1.0, 2.0, 3.0])
                .try_into_detection("camera", Some(&localization))
                .unwrap_err();
            assert_eq!(
                error,
                DetectorFailure::InvalidOutput(DetectionValidationError::NonFiniteLocalization)
            );
        }
    }
}
