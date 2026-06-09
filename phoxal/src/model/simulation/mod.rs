pub mod capability;
pub mod v1;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

const SIMULATION_FILE: &str = "simulation.yaml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "version")]
#[serde(deny_unknown_fields)]
pub enum Simulation {
    #[serde(rename = "v1")]
    V1(v1::Simulation),
}

impl Simulation {
    /// Read the simulation.yaml|simulation.yml file.
    pub fn read_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Self::read_from_string(
            &std::fs::read_to_string(path.join(SIMULATION_FILE)).with_context(|| {
                format!(
                    "failed to read simulation file {}",
                    path.join(SIMULATION_FILE).display()
                )
            })?,
        )
    }

    pub fn read_from_string(string: &str) -> Result<Self> {
        let simulation: Self =
            serde_yaml::from_str(string).with_context(|| "failed to parse simulation")?;
        simulation.validate()?;
        Ok(simulation)
    }

    pub fn write_to_dir(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)
            .with_context(|| format!("failed to create simulation directory {}", path.display()))?;
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(path.join(SIMULATION_FILE), yaml).with_context(|| {
            format!(
                "failed to write simulation file {}",
                path.join(SIMULATION_FILE).display()
            )
        })?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        match self {
            Self::V1(simulation) => simulation.validate(),
        }
    }

    pub fn as_v1(&self) -> Option<&v1::Simulation> {
        match self {
            Self::V1(simulation) => Some(simulation),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Simulation;
    use tempfile::tempdir;

    #[test]
    fn simulation_roundtrips_through_directory() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let simulation_dir = temp_dir.path().join("component");
        let simulation = Simulation::read_from_string(
            r#"
version: v1
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

        simulation.write_to_dir(&simulation_dir)?;
        let loaded = Simulation::read_from_dir(&simulation_dir)?;

        assert!(
            loaded
                .as_v1()
                .expect("supported version")
                .capabilities
                .contains_key("motor")
        );
        assert_eq!(
            loaded
                .as_v1()
                .expect("supported version")
                .links
                .get("wheel_link")
                .and_then(|link| link.contact_material.as_deref()),
            Some("caster_wheel")
        );
        Ok(())
    }

    #[test]
    fn simulation_parses_range_capability() -> anyhow::Result<()> {
        let simulation = Simulation::read_from_string(
            r#"
version: v1
capabilities:
  range:
    kind: range
    sampling_period_hz: 20.0
    noise: 0.02
    resolution: 0.001
"#,
        )?;

        assert!(matches!(
            simulation
                .as_v1()
                .expect("supported version")
                .capabilities
                .get("range"),
            Some(crate::model::simulation::capability::Capability::Range(_))
        ));
        Ok(())
    }

    #[test]
    fn simulation_rejects_invalid_capability_id() {
        let error = Simulation::read_from_string(
            r#"
version: v1
capabilities:
  Bad Id:
    kind: range
    sampling_period_hz: 20.0
"#,
        )
        .expect_err("invalid capability id should fail");

        assert!(error.to_string().contains("valid capability token"));
    }

    #[test]
    fn simulation_rejects_invalid_numeric_capability_config() {
        let error = Simulation::read_from_string(
            r#"
version: v1
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
        assert!(message.contains("sampling_period_hz must be finite and > 0"));
        assert!(message.contains("control_pid must contain exactly 3 terms"));
    }
}
