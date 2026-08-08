//! Versioned authored `simulation.yaml` documents.

pub mod v0;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::source::Violations;
use crate::source::document::{Document, DocumentKind, Origin, SourceError};

/// A versioned authored `simulation.yaml` document; the schema tag selects
/// the variant.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "schema")]
pub enum Manifest {
    #[serde(rename = "phoxal/simulation/v0")]
    V0(v0::Manifest),
}

impl Document for Manifest {
    const KIND: DocumentKind = DocumentKind::Simulation;

    fn check(&self) -> Result<(), Violations> {
        let Self::V0(body) = self;
        body.validate().map_err(Violations::Simulation)
    }
}

impl Manifest {
    /// Parse and validate one simulation document from its exact text.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Parse`] when the text is not a simulation
    /// document, and [`SourceError::Invalid`] when it breaks the grammar's
    /// rules.
    pub fn parse(text: &str) -> Result<Self, SourceError> {
        Self::read_text(text, Origin::Text)
    }

    /// Read and validate the simulation document at `path`, which is either the
    /// document file itself or the directory that holds it.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Read`] when the file cannot be read, plus
    /// whatever [`Manifest::parse`] rejects.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SourceError> {
        Self::read_path(path.as_ref())
    }

    /// Write the document into `directory` as `simulation.yaml`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Write`] when the directory or file cannot be
    /// written.
    pub fn write_to_dir(&self, directory: impl AsRef<Path>) -> Result<(), SourceError> {
        self.write_dir(directory.as_ref())
    }
}

#[cfg(test)]
mod tests {
    use super::Manifest;

    #[test]
    fn simulation_roundtrips_through_directory() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let source = Manifest::parse(
            r#"
schema: phoxal/simulation/v0
capabilities:
  motor:
    kind: motor
    acceleration_radps2: -1.0
    control_pid: [10.0, 0.0, 0.0]
links:
  wheel_link:
    contact_material: caster_wheel
"#,
        )?;
        source.write_to_dir(temp_dir.path())?;
        let Manifest::V0(loaded) = Manifest::load(temp_dir.path())?;
        assert!(loaded.capabilities.contains_key("motor"));
        Ok(())
    }

    #[test]
    fn simulation_parses_range_capability() -> anyhow::Result<()> {
        let Manifest::V0(simulation) = Manifest::parse(
            r#"
schema: phoxal/simulation/v0
capabilities:
  range:
    kind: range
    sampling_period_hz: 20.0
    noise: 0.02
    resolution: 0.001
"#,
        )?;
        assert!(matches!(
            simulation.capabilities.get("range"),
            Some(super::v0::Capability::Range(_))
        ));
        Ok(())
    }

    /// An emergency stop carries no simulation parameters: Webots has no
    /// button, switch, or toggle node, so no simulator drives one.
    #[test]
    fn simulation_parses_emergency_stop_input() -> anyhow::Result<()> {
        let Manifest::V0(simulation) = Manifest::parse(
            r#"
schema: phoxal/simulation/v0
capabilities:
  emergency_stop:
    kind: emergency_stop
"#,
        )?;
        assert!(matches!(
            simulation.capabilities.get("emergency_stop"),
            Some(super::v0::Capability::EmergencyStop)
        ));
        Ok(())
    }

    #[test]
    fn simulation_rejects_invalid_capability_id() {
        let error = Manifest::parse(
            r#"
schema: phoxal/simulation/v0
capabilities:
  Bad Id:
    kind: range
    sampling_period_hz: 20.0
"#,
        )
        .expect_err("invalid capability id should fail");
        assert!(
            error
                .to_string()
                .contains("must use a valid capability token"),
            "{error}"
        );
    }

    #[test]
    fn simulation_rejects_invalid_numeric_capability_config() {
        let error = Manifest::parse(
            r#"
schema: phoxal/simulation/v0
capabilities:
  encoder:
    kind: encoder
    sampling_period_hz: 0.0
  motor:
    kind: motor
    control_pid: [1.0, 2.0]
"#,
        )
        .expect_err("invalid numeric capability config should fail");
        let message = error.to_string();
        assert!(
            message.contains("sampling_period_hz must be finite and > 0"),
            "{message}"
        );
        assert!(
            message.contains("control_pid must contain exactly 3 terms"),
            "{message}"
        );
    }
}
