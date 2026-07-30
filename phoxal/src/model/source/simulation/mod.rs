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
}
