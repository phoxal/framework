//! Versioned authored `component.yaml` documents.

pub mod v0;

use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::source::Violations;
use crate::source::document::{Document, DocumentKind, Origin, SourceError};

/// A versioned authored `component.yaml` document; the schema tag selects the
/// variant.
///
/// The tag names the source language this document is written in, not a
/// framework compatibility identity. Adding a generation means adding a variant
/// here and its own `Manifest::normalize` arm; nothing below the
/// normalization boundary learns that the generation exists.
#[derive(Debug, Clone, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(tag = "schema")]
pub enum Manifest {
    #[serde(rename = "phoxal/component/v0")]
    V0(v0::Manifest),
}

impl Document for Manifest {
    const KIND: DocumentKind = DocumentKind::Component;

    fn check(&self) -> Result<(), Violations> {
        let Self::V0(body) = self;
        body.validate().map_err(Violations::Component)
    }
}

impl Manifest {
    /// Parse and validate one component document from its exact text.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Parse`] when the text is not a component
    /// document, and [`SourceError::Invalid`] when it breaks the grammar's
    /// rules.
    pub fn parse(text: &str) -> Result<Self, SourceError> {
        Self::read_text(text, Origin::Text)
    }

    /// Read and validate the component document at `path`, which is either the
    /// document file itself or the directory that holds it.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Read`] when the file cannot be read, plus
    /// whatever [`Manifest::parse`] rejects.
    pub fn load(path: impl AsRef<Path>) -> Result<Self, SourceError> {
        Self::read_path(path.as_ref())
    }

    /// Write the document into `directory` as `component.yaml`.
    ///
    /// # Errors
    ///
    /// Returns [`SourceError::Write`] when the directory or file cannot be
    /// written.
    pub fn write_to_dir(&self, directory: impl AsRef<Path>) -> Result<(), SourceError> {
        self.write_dir(directory.as_ref())
    }

    /// Validate this document as the definition of `component_type`, so the
    /// rejection names the type an author is actually editing.
    ///
    /// # Errors
    ///
    /// Returns every [`v0::ValidationError`] the document breaks.
    pub fn validate_as(&self, component_type: &str) -> Result<(), Vec<v0::ValidationError>> {
        let Self::V0(body) = self;
        body.validate().map_err(|errors| {
            errors
                .into_iter()
                .map(|error| error.in_component(component_type))
                .collect()
        })
    }

    /// Validate this document as the definition of `component_type` and resolve
    /// its own grammar into the version-independent component the compiler
    /// consumes.
    ///
    /// `root` is the directory the document was read from, so a rejection names
    /// the file an author has to edit. Schema-specific validation stays with
    /// the schema: the compiler below this point has no rules of its own about
    /// how a component document is spelled.
    ///
    /// # Errors
    ///
    /// Returns [`crate::CompileError::Document`] when the document breaks its
    /// own rules, and [`crate::CompileError::Transcode`] when an authored
    /// capability does not adopt into its canonical counterpart.
    pub(crate) fn normalize(
        self,
        component_type: &str,
        root: &Path,
    ) -> Result<crate::normalized::Component, crate::CompileError> {
        self.validate_as(component_type)
            .map_err(|violations| crate::CompileError::Document {
                source: SourceError::Invalid {
                    origin: Origin::File(crate::source::document_path(root, Self::KIND)),
                    violations: Violations::Component(violations),
                },
            })?;
        match self {
            Self::V0(body) => body.normalize(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::Manifest;
    use crate::source::Violations;

    fn violations(text: &str) -> Vec<super::v0::ValidationError> {
        Manifest::parse(text)
            .expect_err("the document must be rejected")
            .violations()
            .and_then(Violations::component)
            .expect("a rejected component document carries component violations")
            .to_vec()
    }

    #[test]
    fn component_roundtrips_through_directory() -> anyhow::Result<()> {
        let temp_dir = tempfile::tempdir()?;
        let source = Manifest::parse(
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

        source.write_to_dir(temp_dir.path())?;
        let Manifest::V0(loaded) = Manifest::load(temp_dir.path())?;
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
        let error = Manifest::load(temp_dir.path()).expect_err("unknown version must fail");
        let message = error.to_string();
        assert!(message.contains("phoxal/component/v0"), "{message}");
        assert!(message.contains("component.yaml"), "{message}");
        Ok(())
    }

    #[test]
    fn component_validates_token_ids() {
        let errors = violations(
            r#"
schema: phoxal/component/v0
capabilities:
  InvalidCapability:
    kind: motor
    command: velocity
    target: { kind: joint, id: j }
"#,
        );
        assert!(
            errors.iter().any(|error| matches!(
                error,
                super::v0::ValidationError::InvalidCapabilityId { capability, .. }
                    if capability == "InvalidCapability"
            )),
            "{errors:?}"
        );
    }

    #[test]
    fn component_parses_and_round_trips_gtin() -> anyhow::Result<()> {
        let manifest = Manifest::parse(
            "schema: phoxal/component/v0\ngtin: \"1234567890123\"\ncapabilities: {}\n",
        )?;
        let Manifest::V0(component) = &manifest;
        assert_eq!(
            component.gtin.as_ref().map(super::v0::Gtin::as_str),
            Some("1234567890123")
        );
        let Manifest::V0(reparsed) = Manifest::parse(&serde_yaml::to_string(&manifest)?)?;
        assert_eq!(
            reparsed.gtin.as_ref().map(super::v0::Gtin::as_str),
            Some("1234567890123")
        );
        Ok(())
    }

    #[test]
    fn component_rejects_non_positive_motor_gear_ratio() {
        let errors = violations(
            r#"
schema: phoxal/component/v0
capabilities:
  motor:
    kind: motor
    command: velocity
    gear_ratio: 0.0
    target: { kind: joint, id: motor_joint }
"#,
        );
        let rendered = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("capabilities.motor.gear_ratio"),
            "{rendered}"
        );
        assert!(rendered.contains("> 0"), "{rendered}");
    }

    #[test]
    fn component_rejects_non_positive_encoder_values() {
        let errors = violations(
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
        );
        let rendered = errors
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n");
        assert!(
            rendered.contains("capabilities.encoder.gear_ratio"),
            "{rendered}"
        );
        assert!(
            rendered.contains("capabilities.encoder.counts_per_revolution"),
            "{rendered}"
        );
        assert!(rendered.matches("> 0").count() >= 2, "{rendered}");
    }

    #[test]
    fn component_rejects_malformed_gtin() {
        let error =
            Manifest::parse("schema: phoxal/component/v0\ngtin: \"123\"\ncapabilities: {}\n")
                .expect_err("13-digit GTIN must be enforced");
        assert!(error.to_string().contains("13 digits"), "got: {error}");
    }

    #[test]
    fn component_without_gtin_is_none_and_omits_on_serialize() -> anyhow::Result<()> {
        let manifest = Manifest::parse("schema: phoxal/component/v0\ncapabilities: {}\n")?;
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
        let Manifest::V0(component) = Manifest::parse(
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
        let component = Manifest::parse(
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
        let Manifest::V0(reparsed) = Manifest::parse(&serde_yaml::to_string(&component)?)?;
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
        let error = Manifest::parse(
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
        .expect_err("a capability carries no profile list");
        assert!(
            error.to_string().contains("unknown field `profiles`"),
            "{error}"
        );
    }
}
