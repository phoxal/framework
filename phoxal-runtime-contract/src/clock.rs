//! The persisted execution time domain shared by bundles and process protocols.

/// The time domain one compiled robot executes in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Clock {
    /// Host boot-anchored time driven by real hardware.
    #[default]
    Real,
    /// Time published by a simulation world authority.
    Simulated,
}
