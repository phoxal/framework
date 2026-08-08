//! Runtime scheduling facts persisted in the compiled bundle.
//!
//! Process launch itself is a strict Clap contract owned by `phoxal`.  This
//! module intentionally contains no environment-variable ABI, launch record,
//! or process parser.  The only value here is the scheduler policy selected by
//! the compiled runtime document.

use serde::{Deserialize, Serialize};

/// The participant scheduler's clock mode.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ClockMode {
    /// A participant derives robot time from the supervisor-provided origin.
    #[default]
    Real,
    /// A participant is driven by the simulation world clock.
    Simulation,
    /// A participant has no scheduled robot-time loop.
    Clockless,
}

impl std::fmt::Display for ClockMode {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::Real => "real",
            Self::Simulation => "simulation",
            Self::Clockless => "clockless",
        })
    }
}
