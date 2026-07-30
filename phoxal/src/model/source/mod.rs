//! Exact authored document schemas.
//!
//! Each document kind owns its version independently. `robot.yaml` v1 can
//! therefore be introduced without forcing `component.yaml` or
//! `simulation.yaml` to advance. Runtime code should use
//! [`crate::model::Robot`]; only schema-aware tools, fixtures, and migration
//! code should import these modules.
//!
//! A second version is added only to the document kind that changes:
//!
//! ```text
//! source/robot/v0/Manifest --explicit normalization--\
//!                                              +--> model::Robot
//! source/robot/v1/Manifest --explicit normalization--/
//!
//! source/component/v0/Manifest --------------------^
//! ```
//!
//! The v0 and v1 robot DTOs stay separately nameable, and each gets an
//! explicit conversion into the same canonical builder input. That normalized
//! builder input is intentionally crate-internal: a future source version is
//! implemented inside `phoxal`, while external version-sensitive tools use
//! [`crate::model::robot::SourceInputs`] and
//! [`crate::model::Robot::try_from_sources`] for the currently supported exact
//! inputs. The in-crate `future_robot_version_can_normalize_with_component_v0`
//! test demonstrates the future sibling conversion. Component and simulation
//! documents remain on v0 until their own grammars change. A public
//! multi-version dispatcher belongs inside that one document-kind module only
//! after the second version actually exists.

pub mod component;
pub mod robot;
pub mod simulation;
