//! Detector head abstraction and the honest deterministic placeholder: the
//! default backend emits no detections, so future detector heads can plug in
//! behind `DetectorHead` without changing the participant IO surface.

use phoxal::api;

pub(crate) struct DetectorInput<'a> {
    pub(crate) camera: &'a api::component::camera::Frame,
    pub(crate) depth: Option<&'a api::component::depth::Frame>,
    pub(crate) frame_id: &'a str,
    pub(crate) stamp_ns: u64,
    pub(crate) localization: Option<&'a api::localize::LocalizationState>,
}

#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RawDetection {
    pub(crate) class_id: String,
    pub(crate) confidence: f32,
    pub(crate) position_m: [f64; 3],
}

pub(crate) trait DetectorHead {
    fn detector_name(&self) -> &'static str;
    fn detect(&mut self, input: DetectorInput<'_>) -> Vec<RawDetection>;
}

pub(crate) struct PlaceholderDetector;

impl DetectorHead for PlaceholderDetector {
    fn detector_name(&self) -> &'static str {
        "deterministic-placeholder"
    }

    fn detect(&mut self, input: DetectorInput<'_>) -> Vec<RawDetection> {
        let _deterministic_input_identity = (
            input.camera.width,
            input.camera.height,
            input.depth.and_then(|depth| depth.width),
            input.frame_id,
            input.stamp_ns,
            input.localization.map(|localize| localize.confidence),
        );
        Vec::new()
    }
}

pub(crate) fn detect_with(
    detector: &mut impl DetectorHead,
    input: DetectorInput<'_>,
) -> Vec<RawDetection> {
    detector.detect(input)
}
