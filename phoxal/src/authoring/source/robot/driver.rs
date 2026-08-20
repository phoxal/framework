//! The `driver:` block a robot document attaches to a component instance.
//!
//! This DTO sits beside the versioned document bodies rather than inside one of
//! them: it is the shape the compiler hands to build tooling on a compiled
//! component instance's driver block, so it belongs to the document family
//! rather than to one generation of its grammar. A generation that spells the
//! block differently converts into this type in its own normalizer, and the
//! compiler below the normalization boundary keeps seeing exactly one driver
//! shape.
//!
//! `robot.yaml` v0 re-exports this type, so an authored v0 document keeps
//! naming it at its established path.
//!
//! The block is two slots with one owner each, and the DTO holds nothing of its
//! own: `connection` is the framework's closed vocabulary, defined once in
//! [`crate::model::connection`] because a driver reads it as a typed value at
//! runtime, and `config` is the driver binary's own configuration, opaque here
//! for the same reason a service config is.
//!
//! [`DriverConfig`] deliberately carries no doc comment: `schemars` renders one
//! into the published editor schema, and the block's own documentation belongs
//! on the two fields a person authors.

use serde::{Deserialize, Serialize};

use crate::model::connection::Connection;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, schemars::JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct DriverConfig {
    /// How this component is wired to the machine.
    pub connection: Connection,
    /// The driver binary's own configuration, and the only thing deserialized
    /// into the type its `#[phoxal::driver(config = …)]` declares.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub config: Option<serde_json::Value>,
}
