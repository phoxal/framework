//! Versioned authored `component.yaml` documents.

pub mod v0;

use std::path::Path;

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};

const COMPONENT_FILE: &str = "component.yaml";

/// A versioned authored `component.yaml` document; the schema tag selects the
/// variant.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "schema")]
pub enum Manifest {
    #[serde(rename = "phoxal/component/v0")]
    V0(v0::Manifest),
}

pub fn read_from_dir(path: impl AsRef<Path>) -> Result<Manifest> {
    let path = path.as_ref().join(COMPONENT_FILE);
    let text = std::fs::read_to_string(&path)
        .with_context(|| format!("failed to read component document {}", path.display()))?;
    read_from_string(&text)
        .with_context(|| format!("failed to parse component document {}", path.display()))
}

pub fn read_from_string(text: &str) -> Result<Manifest> {
    let manifest: Manifest =
        serde_yaml::from_str(text).context("failed to parse component document")?;
    let Manifest::V0(body) = &manifest;
    body.validate()?;
    Ok(manifest)
}

pub fn write_to_dir(manifest: &Manifest, path: impl AsRef<Path>) -> Result<()> {
    let path = path.as_ref();
    std::fs::create_dir_all(path)
        .with_context(|| format!("failed to create component directory {}", path.display()))?;
    let destination = path.join(COMPONENT_FILE);
    std::fs::write(&destination, serde_yaml::to_string(manifest)?).with_context(|| {
        format!(
            "failed to write component document {}",
            destination.display()
        )
    })
}

#[cfg(test)]
mod tests {
    use super::{Manifest, read_from_dir, read_from_string, write_to_dir};

    #[test]
    fn component_roundtrips_through_directory() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let source = read_from_string(
            r#"
schema: phoxal/component/v0
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
        let Manifest::V0(loaded) = loaded;
        assert!(loaded.capabilities.contains_key("motor"));
        Ok(())
    }

    #[test]
    fn component_errors_name_the_file_and_version() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        std::fs::write(
            temp_dir.path().join("component.yaml"),
            "schema: phoxal/component/v1\n",
        )?;
        let error = read_from_dir(temp_dir.path()).expect_err("unknown version must fail");
        let message = format!("{error:#}");
        assert!(message.contains("phoxal/component/v0"));
        assert!(message.contains("component.yaml"));
        Ok(())
    }

    #[test]
    fn component_validates_token_ids() {
        let error = read_from_string(
            r#"
schema: phoxal/component/v0
capabilities:
  InvalidCapability:
    kind: motor
    command: velocity
    target: { kind: joint, id: j }
"#,
        )
        .expect_err("invalid capability id must fail");
        assert!(
            format!("{error:#}").contains("capability id 'InvalidCapability'"),
            "got: {error:#}"
        );
    }

    #[test]
    fn component_parses_and_round_trips_gtin() -> anyhow::Result<()> {
        let manifest = read_from_string(
            "schema: phoxal/component/v0\ngtin: \"1234567890123\"\ncapabilities: {}\n",
        )?;
        let Manifest::V0(component) = &manifest;
        assert_eq!(
            component.gtin.as_ref().map(super::v0::Gtin::as_str),
            Some("1234567890123")
        );
        let yaml = serde_yaml::to_string(&manifest)?;
        let reparsed = read_from_string(&yaml)?;
        let Manifest::V0(reparsed) = reparsed;
        assert_eq!(
            reparsed.gtin.as_ref().map(super::v0::Gtin::as_str),
            Some("1234567890123")
        );
        Ok(())
    }

    #[test]
    fn component_rejects_non_positive_motor_gear_ratio() {
        let error = read_from_string(
            r#"
schema: phoxal/component/v0
capabilities:
  motor:
    kind: motor
    command: velocity
    gear_ratio: 0.0
    target: { kind: joint, id: motor_joint }
"#,
        )
        .expect_err("motor gear_ratio must be enforced");
        let message = format!("{error:#}");
        assert!(message.contains("capabilities.motor.gear_ratio"));
        assert!(message.contains("requires"));
        assert!(message.contains("> 0"), "got: {message}");
    }

    #[test]
    fn component_rejects_non_positive_encoder_values() {
        let error = read_from_string(
            r#"
schema: phoxal/component/v0
capabilities:
  encoder:
    kind: encoder
    publish_rate_hz: 50.0
    gear_ratio: 0.0
    counts_per_revolution: 0
    target: { kind: joint, id: motor_joint }
"#,
        )
        .expect_err("encoder gear_ratio and counts must be enforced");
        let message = format!("{error:#}");
        assert!(message.contains("capabilities.encoder.gear_ratio"));
        assert!(message.contains("capabilities.encoder.counts_per_revolution"));
        assert!(message.matches("> 0").count() >= 2, "got: {message}");
    }

    #[test]
    fn component_rejects_malformed_gtin() {
        let error =
            read_from_string("schema: phoxal/component/v0\ngtin: \"123\"\ncapabilities: {}\n")
                .expect_err("13-digit GTIN must be enforced");
        assert!(format!("{error:#}").contains("13 digits"), "got: {error:#}");
    }

    #[test]
    fn component_without_gtin_is_none_and_omits_on_serialize() -> anyhow::Result<()> {
        let manifest = read_from_string("schema: phoxal/component/v0\ncapabilities: {}\n")?;
        let Manifest::V0(component) = &manifest;
        assert!(component.gtin.is_none());
        let yaml = serde_yaml::to_string(&manifest)?;
        assert!(
            !yaml.contains("gtin"),
            "gtin must be omitted when absent: {yaml}"
        );
        Ok(())
    }

    #[test]
    fn component_parses_range_and_emergency_stop_capabilities() -> anyhow::Result<()> {
        let manifest = read_from_string(
            r#"
schema: phoxal/component/v0
capabilities:
  range:
    kind: range
    publish_rate_hz: 20.0
    min_range_m: 0.04
    max_range_m: 4.0
    field_of_view_rad: 0.471239
    target: { kind: link, id: sensor_link }
  e_stop:
    kind: emergency_stop
    target: { kind: link, id: button_link }
"#,
        )?;
        let Manifest::V0(component) = manifest;
        assert!(matches!(
            component.capabilities.get("range"),
            Some(super::v0::capability::Capability::Range(_))
        ));
        assert!(matches!(
            component.capabilities.get("e_stop"),
            Some(super::v0::capability::Capability::EmergencyStop(_))
        ));
        Ok(())
    }

    #[test]
    fn camera_native_envelope_roundtrips_without_component_profiles() -> anyhow::Result<()> {
        let component = read_from_string(
            r#"
schema: phoxal/component/v0
capabilities:
  rgb:
    kind: camera
    mode: rgb
    publish_rate_hz: 30.0
    width_px: 640
    height_px: 480
    field_of_view_rad: 1.204277
    target: { kind: link, id: camera_link }
  native_only:
    kind: camera
    mode: rgb
    publish_rate_hz: 15.0
    width_px: 320
    height_px: 240
    target: { kind: link, id: native_camera_link }
"#,
        )?;
        let reparsed = read_from_string(&serde_yaml::to_string(&component)?)?;
        let Manifest::V0(reparsed) = reparsed;
        for (id, width, height, rate) in [("rgb", 640, 480, 30.0), ("native_only", 320, 240, 15.0)]
        {
            let Some(super::v0::capability::Capability::Camera(camera)) =
                reparsed.capabilities.get(id)
            else {
                panic!("expected {id} camera capability");
            };
            assert_eq!(
                (camera.width_px, camera.height_px, camera.publish_rate_hz),
                (width, height, rate)
            );
        }
        Ok(())
    }

    #[test]
    fn component_profiles_list_is_rejected() {
        let error = read_from_string(
            r#"
schema: phoxal/component/v0
capabilities:
  rgb:
    kind: camera
    mode: rgb
    publish_rate_hz: 30.0
    width_px: 640
    height_px: 480
    target: { kind: link, id: camera_link }
    profiles:
      publish_rate_hz: 5.0
"#,
        )
        .expect_err("removed profiles field must remain rejected");
        assert!(format!("{error:#}").contains("unknown field `profiles`"));
    }
}
