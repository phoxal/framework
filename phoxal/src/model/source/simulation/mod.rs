//! Versioned authored `simulation.yaml` documents.

pub mod v0;

use std::path::Path;

use anyhow::{Context, Result};

const SIMULATION_FILE: &str = "simulation.yaml";

pub fn read_from_dir(path: impl AsRef<Path>) -> Result<v0::Manifest> {
    let path = path.as_ref().join(SIMULATION_FILE);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read simulation/v0 document {}", path.display()))?;
    read_from_string(&text)
        .with_context(|| format!("failed to parse simulation/v0 document {}", path.display()))
}

pub fn read_from_string(text: &str) -> Result<v0::Manifest> {
    let manifest: v0::Manifest =
        serde_yaml::from_str(text).context("failed to parse simulation/v0 document")?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn write_to_dir(manifest: &v0::Manifest, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create simulation directory {}", path.display()))?;
    let destination = path.join(SIMULATION_FILE);
    std::fs::write(&destination, serde_yaml::to_string(manifest)?).with_context(|| {
        format!(
            "failed to write simulation/v0 document {}",
            destination.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{read_from_dir, read_from_string, write_to_dir};

    #[test]
    fn simulation_roundtrips_through_directory() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let source = read_from_string(
            r#"
schema: simulation/v0
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
        write_to_dir(&source, temp_dir.path())?;
        assert!(
            read_from_dir(temp_dir.path())?
                .capabilities
                .contains_key("motor")
        );
        Ok(())
    }

    #[test]
    fn simulation_parses_range_capability() -> anyhow::Result<()> {
        let simulation = read_from_string(
            r#"
schema: simulation/v0
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
            Some(super::v0::capability::Capability::Range(_))
        ));
        Ok(())
    }

    /// An emergency stop carries no simulation parameters: Webots has no
    /// button, switch, or toggle node, so no simulator drives one.
    #[test]
    fn simulation_parses_emergency_stop_input() -> anyhow::Result<()> {
        let simulation = read_from_string(
            r#"
schema: simulation/v0
capabilities:
  emergency_stop:
    kind: emergency_stop
"#,
        )?;
        assert!(matches!(
            simulation.capabilities.get("emergency_stop"),
            Some(super::v0::capability::Capability::EmergencyStop)
        ));
        Ok(())
    }

    #[test]
    fn simulation_rejects_invalid_capability_id() {
        let error = read_from_string(
            r#"
schema: simulation/v0
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
                .contains("must use a valid capability token")
        );
    }

    #[test]
    fn simulation_rejects_invalid_numeric_capability_config() {
        let error = read_from_string(
            r#"
schema: simulation/v0
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
