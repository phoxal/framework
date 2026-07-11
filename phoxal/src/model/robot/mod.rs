//! Data types for authored source robot manifests.
//!
//! The crate root is the version dispatcher for `robot.yaml`. Schema wire
//! types live under [`v0`]; consumers that need the v0 struct directly can
//! import [`RobotV0`] or [`v0::Robot`].
//!
//! Parsing is strict: every struct denies unknown fields, `serde_yaml`
//! natively rejects duplicate mapping keys and YAML-1.1-only booleans
//! (`yes`/`no`/`on`/`off`) coerced into typed `bool` fields, and
//! [`strict_yaml::check`] additionally rejects anchors/aliases, merge keys,
//! explicit tags, and multi-document streams before the manifest reaches
//! `serde_yaml::from_str`.

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

mod strict_yaml;
pub mod v0;

pub use v0::Robot as RobotV0;
pub use v0::ValidationError;

const ROBOT_FILE: &str = "robot.yaml";

/// Version dispatcher for `robot.yaml`.
///
/// On the wire, manifests use `schema: robot/v0` for the manifest grammar.
/// The single-variant enum is the future-versioning seam: a second schema
/// generation adds a sibling variant here without disturbing `v0::Robot`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[cfg_attr(test, derive(schemars::JsonSchema))]
#[serde(tag = "schema")]
pub enum Robot {
    #[serde(rename = "robot/v0")]
    V0(v0::Robot),
}

impl Robot {
    pub fn read_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let robot = Self::parse_from_dir(path)?;
        robot.validate().map_err(validation_error)?;
        Ok(robot)
    }

    pub fn read_from_string(text: &str) -> Result<Self> {
        let robot = Self::parse_from_string(text)?;
        robot.validate().map_err(validation_error)?;
        Ok(robot)
    }

    pub fn parse_from_dir(path: impl AsRef<Path>) -> Result<Self> {
        let path = path.as_ref();
        Self::parse_from_string(
            &std::fs::read_to_string(path.join(ROBOT_FILE)).with_context(|| {
                format!(
                    "failed to read robot file {}",
                    path.join(ROBOT_FILE).display()
                )
            })?,
        )
    }

    pub fn parse_from_string(text: &str) -> Result<Self> {
        strict_yaml::check(text).context("failed to parse robot")?;
        serde_yaml::from_str(text).with_context(|| "failed to parse robot")
    }

    pub fn write_to_dir(&self, path: impl AsRef<Path>) -> Result<()> {
        let path = path.as_ref();
        std::fs::create_dir_all(path)
            .with_context(|| format!("failed to create robot directory {}", path.display()))?;
        let yaml = serde_yaml::to_string(self)?;
        std::fs::write(path.join(ROBOT_FILE), yaml).with_context(|| {
            format!(
                "failed to write robot file {}",
                path.join(ROBOT_FILE).display()
            )
        })?;
        Ok(())
    }

    pub fn validate(&self) -> std::result::Result<(), Vec<ValidationError>> {
        match self {
            Self::V0(robot) => robot.validate(),
        }
    }

    pub fn validate_with(
        &self,
        platform_participant_names: &[&str],
    ) -> std::result::Result<(), Vec<ValidationError>> {
        match self {
            Self::V0(robot) => robot.validate_with(platform_participant_names),
        }
    }

    #[must_use]
    pub fn as_v0(&self) -> &v0::Robot {
        match self {
            Self::V0(robot) => robot,
        }
    }

    #[must_use]
    pub fn into_v0(self) -> v0::Robot {
        match self {
            Self::V0(robot) => robot,
        }
    }
}

fn validation_error(errors: Vec<ValidationError>) -> anyhow::Error {
    let message = errors
        .iter()
        .map(ToString::to_string)
        .collect::<Vec<_>>()
        .join("\n");
    anyhow::anyhow!("Robot errors:\n{message}")
}

/// The editor-facing `robot.yaml` JSON Schema is a structural grammar aid.
/// `Robot` and every serde-shaped type it reaches carry a
/// `#[cfg_attr(test, derive(schemars::JsonSchema))]`, so the schema is derived
/// from the manifest structs rather than maintained separately.
/// `schema_matches_model` fails when the checked-in
/// `examples/robot.schema.json` stops matching that derived serde shape.
/// The schema is not an executable specification.
/// It does not cover `strict_yaml::check`, semantic [`Robot::validate`]
/// constraints, custom `FromStr` validation, or cross-file component and URDF
/// constraints.
///
/// Kept test-only (not a normal dependency) on purpose: `schemars` and
/// `jsonschema` are dev-dependencies of `phoxal` (see `phoxal/Cargo.toml`),
/// so the published crate carries no schema-generation code or its
/// dependency weight - only `cargo test` regenerates and checks the schema.
#[cfg(test)]
mod schema_guard {
    use super::Robot;

    /// Path to the checked-in JSON Schema, relative to this crate's manifest
    /// directory (`phoxal/`).
    const SCHEMA_PATH: &str = "../examples/robot.schema.json";

    fn generated_schema_json() -> String {
        let schema = schemars::schema_for!(Robot);
        serde_json::to_string_pretty(&schema).expect("schema serializes to JSON") + "\n"
    }

    fn schema_path() -> std::path::PathBuf {
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join(SCHEMA_PATH)
    }

    /// Regenerate-and-diff guard: run `cargo test -p phoxal schema_guard::print_schema
    /// -- --ignored --nocapture > examples/robot.schema.json` (from the
    /// `framework/phoxal` directory) and re-check the file in when this fails.
    #[test]
    fn schema_matches_model() {
        let generated = generated_schema_json();
        let checked_in = std::fs::read_to_string(schema_path()).unwrap_or_default();

        assert_eq!(
            generated,
            checked_in,
            "examples/robot.schema.json is stale relative to the schemars-derived \
             schema for phoxal::model::robot::Robot. Regenerate it with:\n\n  \
             cargo test -p phoxal schema_guard::print_schema -- --ignored --nocapture \
             > {}\n\nthen re-check the file in.",
            schema_path().display()
        );
    }

    /// Not an assertion - an opt-in helper (`--ignored`) that prints the
    /// freshly generated schema so `schema_matches_model`'s failure message
    /// can be piped straight into `examples/robot.schema.json`.
    #[test]
    #[ignore = "prints the schema; run explicitly to regenerate examples/robot.schema.json"]
    fn print_schema() {
        print!("{}", generated_schema_json());
    }

    fn validator_for_model() -> jsonschema::Validator {
        let schema = schemars::schema_for!(Robot);
        jsonschema::validator_for(&serde_json::to_value(&schema).expect("schema is valid JSON"))
            .expect("generated schema should itself be a valid JSON Schema")
    }

    /// The schema must actually accept the checked-in example manifest - proof
    /// that the drift guard tracks a schema that validates real content, not
    /// just two frozen blobs that happen to match each other.
    #[test]
    fn schema_validates_the_hello_rover_example() {
        let manifest_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"));
        let robot_yaml =
            std::fs::read_to_string(manifest_dir.join("../examples/hello-rover/robot.yaml"))
                .expect("examples/hello-rover/robot.yaml should be readable");
        let value: serde_json::Value = serde_yaml::from_str(&robot_yaml)
            .expect("examples/hello-rover/robot.yaml should parse as YAML");

        let validator = validator_for_model();
        let errors: Vec<_> = validator
            .iter_errors(&value)
            .map(|error| error.to_string())
            .collect();
        assert!(
            errors.is_empty(),
            "examples/hello-rover/robot.yaml should validate against robot.schema.json: {errors:?}"
        );
    }

    /// A manifest with an unknown root key must be rejected - proof that
    /// `deny_unknown_fields` really propagates into the schema's
    /// `additionalProperties: false`, not just that the happy path validates.
    #[test]
    fn schema_rejects_unknown_root_key() {
        let yaml = r#"
schema: robot/v0
robot:
  id: test-bot
  namespace: dev
  kinematic:
    kind: omnidirectional
    actuators: []
    encoders: []
  components: {}
not_a_real_key: {}
"#;
        let value: serde_json::Value = serde_yaml::from_str(yaml).expect("valid YAML");

        assert!(
            !validator_for_model().is_valid(&value),
            "schema should reject an unknown root key"
        );
    }
}
