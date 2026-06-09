use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub mod v1;

const COMPONENT_FILE: &str = "component.yaml";

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "version")]
pub enum Component {
    #[serde(rename = "v1")]
    V1(v1::Component),
}

impl Component {
    pub fn read_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        let component_string =
            std::fs::read_to_string(path.join(COMPONENT_FILE)).with_context(|| {
                format!(
                    "failed to read component file {}",
                    path.join(COMPONENT_FILE).display()
                )
            })?;
        let component: Self =
            serde_yaml::from_str(&component_string).with_context(|| "failed to parse component")?;
        let component_id = path
            .file_name()
            .and_then(|value| value.to_str())
            .unwrap_or("<component>");
        component.validate_for_component(component_id)?;
        Ok(component)
    }

    pub fn read_from_string(string: &str) -> Result<Self> {
        let component: Self =
            serde_yaml::from_str(string).with_context(|| "failed to parse component")?;
        component.validate()?;
        Ok(component)
    }

    pub fn write_to_dir(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)
            .with_context(|| format!("failed to create component directory {}", path.display()))?;
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(path.join(COMPONENT_FILE), yaml).with_context(|| {
            format!(
                "failed to write component file {}",
                path.join(COMPONENT_FILE).display()
            )
        })?;
        Ok(())
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_for_component("<inline>")
    }

    pub fn validate_for_component(&self, component_id: &str) -> Result<()> {
        match self {
            Self::V1(component) => component.validate_for_component(component_id),
        }
    }

    pub fn as_v1(&self) -> Option<&v1::Component> {
        match self {
            Self::V1(component) => Some(component),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Component;
    use crate::model::component::v1::capability::Capability as V1Capability;
    use tempfile::tempdir;

    #[test]
    fn component_roundtrips_through_directory() -> anyhow::Result<()> {
        let temp_dir = tempdir()?;
        let component_dir = temp_dir.path().join("component");
        let component = Component::read_from_string(
            r#"
version: v1
capabilities:
  motor:
    kind: motor
    command: velocity
    target:
      kind: joint
      id: motor_joint
"#,
        )?;

        component.write_to_dir(&component_dir)?;
        let loaded = Component::read_from_dir(&component_dir)?;

        assert!(
            loaded
                .as_v1()
                .expect("supported version")
                .capabilities
                .contains_key("motor")
        );
        Ok(())
    }

    #[test]
    fn component_validates_token_ids() -> anyhow::Result<()> {
        let result = Component::read_from_string(
            r#"
version: v1
capabilities:
  InvalidCapability:
    kind: motor
    command: velocity
    target: { kind: joint, id: j }
"#,
        );
        assert!(result.is_err());
        assert!(
            result
                .unwrap_err()
                .to_string()
                .contains("capability id 'InvalidCapability'")
        );

        Ok(())
    }

    #[test]
    fn component_parses_and_round_trips_gtin() -> anyhow::Result<()> {
        let component = Component::read_from_string(
            "version: v1\ngtin: \"1234567890123\"\ncapabilities: {}\n",
        )?;
        assert_eq!(
            component
                .as_v1()
                .expect("supported version")
                .gtin
                .as_ref()
                .map(crate::model::component::v1::Gtin::as_str),
            Some("1234567890123")
        );
        let yaml = serde_yaml::to_string(&component)?;
        let reparsed = Component::read_from_string(&yaml)?;
        assert_eq!(
            reparsed
                .as_v1()
                .expect("supported version")
                .gtin
                .as_ref()
                .map(crate::model::component::v1::Gtin::as_str),
            Some("1234567890123")
        );
        Ok(())
    }

    #[test]
    fn component_rejects_non_positive_motor_gear_ratio() {
        let result = Component::read_from_string(
            r#"
version: v1
capabilities:
  motor:
    kind: motor
    command: velocity
    gear_ratio: 0.0
    target: { kind: joint, id: motor_joint }
"#,
        );
        let error = result.expect_err("motor gear_ratio must be enforced");
        assert!(
            format!("{error:#}").contains("capabilities.motor.gear_ratio must be > 0"),
            "got: {error:#}"
        );
    }

    #[test]
    fn component_rejects_non_positive_encoder_values() {
        let result = Component::read_from_string(
            r#"
version: v1
capabilities:
  encoder:
    kind: encoder
    publish_rate_hz: 50.0
    gear_ratio: 0.0
    counts_per_revolution: 0
    target: { kind: joint, id: motor_joint }
"#,
        );
        let error = result.expect_err("encoder gear_ratio and counts must be enforced");
        let message = format!("{error:#}");
        assert!(
            message.contains("capabilities.encoder.gear_ratio must be > 0"),
            "got: {message}"
        );
        assert!(
            message.contains("capabilities.encoder.counts_per_revolution must be > 0"),
            "got: {message}"
        );
    }

    #[test]
    fn component_rejects_malformed_gtin() {
        let result = Component::read_from_string("version: v1\ngtin: \"123\"\ncapabilities: {}\n");
        let error = result.expect_err("13-digit GTIN must be enforced");
        assert!(format!("{error:#}").contains("13 digits"), "got: {error:#}");
    }

    #[test]
    fn component_without_gtin_is_none_and_omits_on_serialize() -> anyhow::Result<()> {
        let component = Component::read_from_string("version: v1\ncapabilities: {}\n")?;
        assert!(component.as_v1().expect("supported version").gtin.is_none());
        let yaml = serde_yaml::to_string(&component)?;
        assert!(
            !yaml.contains("gtin"),
            "gtin must be omitted when absent: {yaml}"
        );
        Ok(())
    }

    #[test]
    fn component_parses_range_capability() -> anyhow::Result<()> {
        let component = Component::read_from_string(
            r#"
version: v1
capabilities:
  range:
    kind: range
    publish_rate_hz: 20.0
    min_range_m: 0.04
    max_range_m: 4.0
    field_of_view_rad: 0.471239
    target:
      kind: link
      id: sensor_link
"#,
        )?;

        assert!(matches!(
            component
                .as_v1()
                .expect("supported version")
                .capabilities
                .get("range"),
            Some(crate::model::component::v1::capability::Capability::Range(
                _
            ))
        ));
        Ok(())
    }

    #[test]
    fn component_parses_emergency_stop_capability() -> anyhow::Result<()> {
        let component = Component::read_from_string(
            r#"
version: v1
capabilities:
  e_stop:
    kind: emergency_stop
    target:
      kind: link
      id: button_link
"#,
        )?;

        assert!(matches!(
            component
                .as_v1()
                .expect("supported version")
                .capabilities
                .get("e_stop"),
            Some(crate::model::component::v1::capability::Capability::EmergencyStop(_))
        ));
        Ok(())
    }

    #[test]
    fn camera_native_envelope_roundtrips_without_component_profiles() -> anyhow::Result<()> {
        let component = Component::read_from_string(
            r#"
version: v1
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

        let serialized = serde_yaml::to_string(&component)?;
        let reparsed = Component::read_from_string(&serialized)?;
        let capabilities = &reparsed.as_v1().expect("supported version").capabilities;

        match capabilities.get("rgb") {
            Some(V1Capability::Camera(camera)) => {
                assert_eq!(camera.width_px, 640);
                assert_eq!(camera.height_px, 480);
                assert_eq!(camera.publish_rate_hz, 30.0);
            }
            _ => panic!("expected rgb camera capability"),
        }

        match capabilities.get("native_only") {
            Some(V1Capability::Camera(camera)) => {
                assert_eq!(camera.width_px, 320);
                assert_eq!(camera.height_px, 240);
                assert_eq!(camera.publish_rate_hz, 15.0);
            }
            _ => panic!("expected native_only camera capability"),
        }

        Ok(())
    }

    #[test]
    fn component_profiles_list_is_rejected() {
        let result = Component::read_from_string(
            r#"
version: v1
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
        width_px: 320
        height_px: 240
        encoding: jpeg
"#,
        );

        let message = format!("{:#}", result.unwrap_err());
        assert!(message.contains("unknown field `profiles`"));
    }
}
