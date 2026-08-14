/// An exact component-capability source reference.
#[derive(
    phoxal_macros::DescribeWire,
    serde::Serialize,
    serde::Deserialize,
    Debug,
    Clone,
    PartialEq,
    Eq,
    PartialOrd,
    Ord,
    Hash,
)]
#[serde(try_from = "String", into = "String")]
pub struct SourceRef(String);

impl SourceRef {
    pub fn parse(value: impl Into<String>) -> Result<Self, InvalidSourceRef> {
        value.into().try_into()
    }

    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidSourceRef(String);

impl std::fmt::Display for InvalidSourceRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "invalid source reference '{}'; expected component.capability",
            self.0
        )
    }
}

impl std::error::Error for InvalidSourceRef {}

impl TryFrom<String> for SourceRef {
    type Error = InvalidSourceRef;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let Some((component, capability)) = value.split_once('.') else {
            return Err(InvalidSourceRef(value));
        };
        if value.matches('.').count() != 1
            || !valid_source_segment(component)
            || !valid_source_segment(capability)
        {
            return Err(InvalidSourceRef(value));
        }
        Ok(Self(value))
    }
}

impl From<SourceRef> for String {
    fn from(value: SourceRef) -> Self {
        value.0
    }
}

fn valid_source_segment(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}

const MAX_IDENTIFIER_LEN: usize = 128;
const MAX_DETECTIONS: usize = 4_096;

/// A detection with validated identity, probability and
/// position fields.
///
/// The fields stay private so a detector cannot create a value which bypasses
/// the same checks used by wire deserialization. Tracking is deliberately
/// assigned after construction by the perception service.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "DetectionWire")]
pub struct Detection {
    class_id: String,
    confidence: f32,
    position_m: [f64; 3],
    frame_id: String,
    track_id: Option<u64>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DetectionWire {
    class_id: String,
    confidence: f32,
    position_m: [f64; 3],
    frame_id: String,
    track_id: Option<u64>,
}

/// Why a detection could not be admitted to the current wire contract.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InvalidDetection {
    InvalidClassId,
    InvalidConfidence,
    InvalidPosition,
    InvalidFrameId,
}

impl std::fmt::Display for InvalidDetection {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let message = match self {
            Self::InvalidClassId => "perception class id must be a bounded non-empty identifier",
            Self::InvalidConfidence => "perception confidence must be finite and in [0, 1]",
            Self::InvalidPosition => "perception position must contain only finite coordinates",
            Self::InvalidFrameId => "perception frame id must be a bounded non-empty identifier",
        };
        formatter.write_str(message)
    }
}

impl std::error::Error for InvalidDetection {}

impl Detection {
    /// Construct a detection after checking all detector-controlled fields.
    pub fn try_new(
        class_id: impl Into<String>,
        confidence: f32,
        position_m: [f64; 3],
        frame_id: impl Into<String>,
    ) -> Result<Self, InvalidDetection> {
        let class_id = class_id.into();
        if !valid_identifier(&class_id) {
            return Err(InvalidDetection::InvalidClassId);
        }
        if !confidence.is_finite() || !(0.0..=1.0).contains(&confidence) {
            return Err(InvalidDetection::InvalidConfidence);
        }
        if !position_m.iter().all(|coordinate| coordinate.is_finite()) {
            return Err(InvalidDetection::InvalidPosition);
        }
        let frame_id = frame_id.into();
        if !valid_identifier(&frame_id) {
            return Err(InvalidDetection::InvalidFrameId);
        }
        Ok(Self {
            class_id,
            confidence,
            position_m,
            frame_id,
            track_id: None,
        })
    }

    /// The detector's stable class identifier.
    #[must_use]
    pub fn class_id(&self) -> &str {
        &self.class_id
    }

    /// The detector confidence as a probability in the closed unit interval.
    #[must_use]
    pub fn confidence(&self) -> f32 {
        self.confidence
    }

    /// The position expressed in [`Self::frame_id`].
    #[must_use]
    pub fn position_m(&self) -> [f64; 3] {
        self.position_m
    }

    /// The frame in which [`Self::position_m`] is expressed.
    #[must_use]
    pub fn frame_id(&self) -> &str {
        &self.frame_id
    }

    /// The service-assigned track identifier, if tracking has associated one.
    #[must_use]
    pub fn track_id(&self) -> Option<u64> {
        self.track_id
    }

    /// Assign or clear the service-owned track identifier.
    pub fn set_track_id(&mut self, track_id: Option<u64>) {
        self.track_id = track_id;
    }
}

impl TryFrom<DetectionWire> for Detection {
    type Error = InvalidDetection;

    fn try_from(value: DetectionWire) -> Result<Self, Self::Error> {
        let mut detection = Self::try_new(
            value.class_id,
            value.confidence,
            value.position_m,
            value.frame_id,
        )?;
        detection.track_id = value.track_id;
        Ok(detection)
    }
}

/// One source-captured perception batch. `captured_at` is copied from the
/// selected camera measurement's provenance; it is not the perception step's
/// publication instant.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "DetectionsWire")]
pub struct Detections {
    source: SourceRef,
    captured_at: ::phoxal_bus::TimeWindow,
    detections: Vec<Detection>,
}

#[derive(Clone, Debug, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct DetectionsWire {
    source: SourceRef,
    captured_at: ::phoxal_bus::TimeWindow,
    detections: Vec<Detection>,
}

/// Why a perception detection batch could not be admitted to the wire.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InvalidDetections {
    TooManyDetections,
}

impl std::fmt::Display for InvalidDetections {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("perception detection batch exceeds its bounded size")
    }
}

impl std::error::Error for InvalidDetections {}

impl Detections {
    /// Construct a source-captured detection batch with bounded memory.
    pub fn try_new(
        source: SourceRef,
        captured_at: ::phoxal_bus::TimeWindow,
        detections: Vec<Detection>,
    ) -> Result<Self, InvalidDetections> {
        if detections.len() > MAX_DETECTIONS {
            return Err(InvalidDetections::TooManyDetections);
        }
        Ok(Self {
            source,
            captured_at,
            detections,
        })
    }

    /// The component-capability source that captured this batch.
    #[must_use]
    pub fn source(&self) -> &SourceRef {
        &self.source
    }

    /// The source capture window preserved from the input measurement.
    #[must_use]
    pub fn captured_at(&self) -> ::phoxal_bus::TimeWindow {
        self.captured_at
    }

    /// The detections in source order.
    #[must_use]
    pub fn detections(&self) -> &[Detection] {
        &self.detections
    }
}

impl TryFrom<DetectionsWire> for Detections {
    type Error = InvalidDetections;

    fn try_from(value: DetectionsWire) -> Result<Self, Self::Error> {
        Self::try_new(value.source, value.captured_at, value.detections)
    }
}

/// Why the perception participant cannot provide a healthy batch.
#[derive(
    phoxal_macros::DescribeWire,
    Copy,
    Eq,
    Clone,
    Debug,
    PartialEq,
    serde::Serialize,
    serde::Deserialize,
)]
#[serde(rename_all = "snake_case")]
pub enum HealthReason {
    MissingCamera,
    StaleCamera,
    InvalidCamera,
    DetectorFailure,
    BackendUnavailable,
    PublicationFailure,
    ManagedInputFailure,
}

/// The perception participant's exclusive published health.
///
/// The public value is a struct with private fields so detector identity is
/// validated for both constructor-created and deserialized states. The wire
/// shape remains the externally-tagged `Healthy`/`Unhealthy` enum the
/// contract carries.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "StateWire", into = "StateWire")]
pub struct State {
    detector: String,
    reason: Option<HealthReason>,
}

#[derive(phoxal_macros::DescribeWire, Clone, Debug, serde::Serialize, serde::Deserialize)]
enum StateWire {
    Healthy(HealthyStateWire),
    Unhealthy(UnhealthyStateWire),
}

#[derive(phoxal_macros::DescribeWire, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct HealthyStateWire {
    detector: String,
}

#[derive(phoxal_macros::DescribeWire, Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct UnhealthyStateWire {
    detector: String,
    reason: HealthReason,
}

/// Why a detector health state could not be created.
#[derive(Copy, Clone, Debug, PartialEq, Eq)]
pub enum InvalidState {
    InvalidDetectorId,
}

impl std::fmt::Display for InvalidState {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("perception detector id must be a bounded non-empty identifier")
    }
}

impl std::error::Error for InvalidState {}

impl State {
    /// Construct a healthy state for a validated detector identity.
    pub fn healthy(detector: impl Into<String>) -> Result<Self, InvalidState> {
        Self::try_new(detector, None)
    }

    /// Construct an unhealthy state for a validated detector identity.
    pub fn unhealthy(
        detector: impl Into<String>,
        reason: HealthReason,
    ) -> Result<Self, InvalidState> {
        Self::try_new(detector, Some(reason))
    }

    fn try_new(
        detector: impl Into<String>,
        reason: Option<HealthReason>,
    ) -> Result<Self, InvalidState> {
        let detector = detector.into();
        if !valid_identifier(&detector) {
            return Err(InvalidState::InvalidDetectorId);
        }
        Ok(Self { detector, reason })
    }

    /// The detector identity that produced this health state.
    #[must_use]
    pub fn detector(&self) -> &str {
        &self.detector
    }

    /// Whether this state reports successful detector processing.
    #[must_use]
    pub fn is_healthy(&self) -> bool {
        self.reason.is_none()
    }

    /// The unhealthy reason, if this is an unhealthy state.
    #[must_use]
    pub fn health_reason(&self) -> Option<HealthReason> {
        self.reason
    }
}

impl From<State> for StateWire {
    fn from(value: State) -> Self {
        match value.reason {
            None => Self::Healthy(HealthyStateWire {
                detector: value.detector,
            }),
            Some(reason) => Self::Unhealthy(UnhealthyStateWire {
                reason,
                detector: value.detector,
            }),
        }
    }
}

impl TryFrom<StateWire> for State {
    type Error = InvalidState;

    fn try_from(value: StateWire) -> Result<Self, Self::Error> {
        let (detector, reason) = match value {
            StateWire::Healthy(HealthyStateWire { detector }) => (detector, None),
            StateWire::Unhealthy(UnhealthyStateWire { reason, detector }) => {
                (detector, Some(reason))
            }
        };
        Self::try_new(detector, reason)
    }
}

fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_IDENTIFIER_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'))
}

phoxal_macros::protocol_fragment! {
    path robot / perception;

    detections: State<Detections>;
    state: State<State>;
}
