//! Canonical component-capability source references shared by wire domains.

/// The exact camera capability reference accepted by `video/open`.
///
/// This is API-owned because the contract crate intentionally does not depend
/// upward on the runtime model. It has the same canonical dotted spelling as
/// the model reference (`component.capability`), but deserialization rejects
/// malformed or aliased source names before a query reaches a service.
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(try_from = "String", into = "String")]
pub struct VideoSourceRef(String);

/// A validated dotted capability source reference.
///
/// This is the generic form of [`VideoSourceRef`]. The older name remains a
/// type alias so the published v0.1 video request keeps the same Rust and wire
/// surface while newer contracts can reuse the exact same validation without
/// depending on the runtime model crate.
pub type SourceRef = VideoSourceRef;

impl VideoSourceRef {
    /// Construct an already validated wire reference.
    pub fn parse(value: impl Into<String>) -> Result<Self, InvalidVideoSourceRef> {
        value.into().try_into()
    }

    /// The canonical dotted source spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a `VideoSourceRef` failed its exact wire grammar.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidVideoSourceRef(String);

/// Generic spelling of the validation error for [`SourceRef`].
pub type InvalidSourceRef = InvalidVideoSourceRef;

impl std::fmt::Display for InvalidVideoSourceRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            formatter,
            "invalid video source reference '{}'; expected component.capability",
            self.0
        )
    }
}

impl std::error::Error for InvalidVideoSourceRef {}

impl TryFrom<String> for VideoSourceRef {
    type Error = InvalidVideoSourceRef;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        let Some((component, capability)) = value.split_once('.') else {
            return Err(InvalidVideoSourceRef(value));
        };
        if value.matches('.').count() != 1
            || !valid_video_source_segment(component)
            || !valid_video_source_segment(capability)
        {
            return Err(InvalidVideoSourceRef(value));
        }
        Ok(Self(value))
    }
}

impl From<VideoSourceRef> for String {
    fn from(value: VideoSourceRef) -> Self {
        value.0
    }
}

fn valid_video_source_segment(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}
