//! The one document a compiled robot is persisted as.
//!
//! `manifest.json` at the root of a bundle carries exactly one [`Robot`] under
//! a schema tag. There is no second persisted document beside it: the process
//! set is derived from the robot (`brain`, every service key, every component
//! key), and every participant's own configuration is read from the same body.
//!
//! The document type lives here rather than in `phoxal-bundle` so that the
//! compiler which writes it and the reader which loads it both reach it through
//! the crate that owns the body. `phoxal-bundle` may not depend on
//! `phoxal-manifest`, and this is what keeps that direction honest.

use serde::{Deserialize, Serialize};

use crate::robot::Robot;

/// The only manifest schema this framework train reads or writes.
///
/// A serde attribute cannot name a constant, so the spelling below is written
/// twice: once on [`ManifestDocument`]'s `rename` and once here.
/// `the_schema_tag_is_written_once_on_the_wire` serializes a real document
/// against it, so a drift between the two fails a test rather than shipping.
pub const MANIFEST_SCHEMA: &str = "phoxal/manifest/v0";

/// The schema-tagged compiled robot.
///
/// The tag is a format discriminator, not a compatibility identity: a reader
/// refuses a generation it does not implement instead of guessing at the body.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Serialize)]
#[serde(tag = "schema", deny_unknown_fields)]
pub enum ManifestDocument {
    #[serde(rename = "phoxal/manifest/v0")]
    V0(Robot),
}

impl ManifestDocument {
    /// Wrap one already-validated robot.
    #[must_use]
    pub const fn new(robot: Robot) -> Self {
        Self::V0(robot)
    }

    /// The compiled robot this document carries.
    #[must_use]
    pub const fn robot(&self) -> &Robot {
        match self {
            Self::V0(robot) => robot,
        }
    }

    /// Take the compiled robot out of the document.
    #[must_use]
    pub fn into_robot(self) -> Robot {
        match self {
            Self::V0(robot) => robot,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{MANIFEST_SCHEMA, ManifestDocument};
    use crate::builder::RobotBuilder;

    fn document() -> ManifestDocument {
        ManifestDocument::new(
            RobotBuilder::new("rover")
                .component_type("rgbd", |camera| camera.camera("rgb", "lens"))
                .component("front_camera", "rgbd")
                .build()
                .expect("a valid canonical robot"),
        )
    }

    /// The tag serde writes and the constant every reader spells are one value,
    /// checked against a real serialized document rather than asserted.
    #[test]
    fn the_schema_tag_is_written_once_on_the_wire() {
        let json = serde_json::to_value(document()).expect("a manifest document serializes");
        assert_eq!(json["schema"], serde_json::Value::from(MANIFEST_SCHEMA));
    }

    #[test]
    fn a_document_round_trips_through_its_tag() {
        let json = serde_json::to_value(document()).expect("a manifest document serializes");
        let decoded: ManifestDocument =
            serde_json::from_value(json).expect("its own output must parse");
        assert_eq!(decoded.robot().id().as_str(), "rover");
        assert!(decoded.robot().component("front_camera").is_some());
    }

    /// A tag this train does not implement is refused before any field of the
    /// body is read.
    #[test]
    fn an_unknown_schema_tag_is_rejected() {
        let mut json = serde_json::to_value(document()).expect("a manifest document serializes");
        json["schema"] = serde_json::Value::from("phoxal/manifest/v1");
        assert!(serde_json::from_value::<ManifestDocument>(json).is_err());
    }
}
