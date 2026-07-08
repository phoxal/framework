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
