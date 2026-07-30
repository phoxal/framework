//! Versioned authored `component.yaml` documents.

pub mod v0;

use std::path::Path;

use anyhow::{Context, Result};

const COMPONENT_FILE: &str = "component.yaml";

pub fn read_from_dir(path: impl AsRef<Path>) -> Result<v0::Manifest> {
    let path = path.as_ref().join(COMPONENT_FILE);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read component/v0 document {}", path.display()))?;
    read_from_string(&text)
        .with_context(|| format!("failed to parse component/v0 document {}", path.display()))
}

pub fn read_from_string(text: &str) -> Result<v0::Manifest> {
    let manifest: v0::Manifest =
        serde_yaml::from_str(text).context("failed to parse component/v0 document")?;
    manifest.validate()?;
    Ok(manifest)
}

pub fn write_to_dir(manifest: &v0::Manifest, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create component directory {}", path.display()))?;
    let destination = path.join(COMPONENT_FILE);
    std::fs::write(&destination, serde_yaml::to_string(manifest)?).with_context(|| {
        format!(
            "failed to write component/v0 document {}",
            destination.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{read_from_dir, read_from_string, write_to_dir};

    #[test]
    fn component_roundtrips_through_directory() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let source = read_from_string(
            r#"
schema: component/v0
capabilities:
  motor:
    kind: motor
    command: velocity
    target:
      kind: joint
      id: motor_joint
"#,
        )?;

        write_to_dir(&source, temp_dir.path())?;
        let loaded = read_from_dir(temp_dir.path())?;
        assert!(loaded.capabilities.contains_key("motor"));
        Ok(())
    }

    #[test]
    fn component_errors_name_the_file_and_version() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        std::fs::write(
            temp_dir.path().join("component.yaml"),
            "schema: component/v1\n",
        )?;
        let error = read_from_dir(temp_dir.path()).expect_err("unknown version must fail");
        let message = format!("{error:#}");
        assert!(message.contains("component/v0"));
        assert!(message.contains("component.yaml"));
        Ok(())
    }
}
