//! Data types for authored source robot manifests.
//!
//! The crate root is the version dispatcher for `robot.yaml`. Schema wire
//! types live under [`v1`]; consumers that need the v1 struct directly can
//! import [`RobotV1`] or [`v1::Robot`].

use anyhow::{Context, Result};
use serde::{Deserialize, Serialize};
use std::path::Path;

pub mod v1;

pub use v1::Robot as RobotV1;
pub use v1::ValidationError;

const ROBOT_FILE: &str = "robot.yaml";

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "version")]
pub enum Robot {
    #[serde(rename = "v1")]
    V1(v1::Robot),
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
            Self::V1(robot) => robot.validate(),
        }
    }

    pub fn validate_with(
        &self,
        platform_runtime_names: &[&str],
    ) -> std::result::Result<(), Vec<ValidationError>> {
        match self {
            Self::V1(robot) => robot.validate_with(platform_runtime_names),
        }
    }

    #[must_use]
    pub fn as_v1(&self) -> &v1::Robot {
        match self {
            Self::V1(robot) => robot,
        }
    }

    #[must_use]
    pub fn into_v1(self) -> v1::Robot {
        match self {
            Self::V1(robot) => robot,
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
