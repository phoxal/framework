//! Canonical component-capability source references shared by wire domains.

/// An exact component-capability source reference.
///
/// This is API-owned because the contract crate intentionally does not depend
/// upward on the runtime model. It has the same canonical dotted spelling as
/// the model reference (`component.capability`), but deserialization rejects
/// malformed or aliased source names before a query reaches a service.
#[derive(
    serde::Serialize, serde::Deserialize, Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash,
)]
#[serde(try_from = "String", into = "String")]
pub struct SourceRef(String);

impl SourceRef {
    /// Construct an already validated wire reference.
    pub fn parse(value: impl Into<String>) -> Result<Self, InvalidSourceRef> {
        value.into().try_into()
    }

    /// The canonical dotted source spelling.
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Why a [`SourceRef`] failed its exact wire grammar.
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
            || !valid_video_source_segment(component)
            || !valid_video_source_segment(capability)
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

fn valid_video_source_segment(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || byte == b'_' || byte == b'-'
        })
}
