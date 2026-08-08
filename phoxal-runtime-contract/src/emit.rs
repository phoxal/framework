//! The one sanctioned writer of the embedded participant-metadata document.
//!
//! [`ParticipantMetadata`](crate::metadata::ParticipantMetadata) is
//! deserialize-only, so the document's serialized shape is defined exactly
//! once, here, by [`ParticipantMetadataRecord`]. Every field is a typed version
//! identity; no writer anywhere restates a version as a string literal.
//!
//! A role macro cannot call `serde_json` though: the record it emits lands in a
//! `#[link_section]` static, whose length must be a constant, and its
//! `config_schema` is only known after `rustc` const-evaluates the recursive
//! `ParticipantConfig::SCHEMA_JSON` tree in the participant's own crate. So the
//! const-eval path is
//! [`participant_metadata_json!`](crate::participant_metadata_json), which
//! composes the same document from the same typed values through
//! `const_format`. The two are one writer in two evaluation modes, and
//! `the_const_writer_emits_exactly_what_the_typed_record_serializes` fails if
//! they ever disagree.

use serde::Serialize;

use crate::metadata::{ParticipantKind, ParticipantRequirement, ParticipantSchemas};
use crate::version::RobotApi;

/// `const_format::concatcp!`, made reachable as `$crate::emit::concatcp!`.
///
/// [`participant_metadata_json!`](crate::participant_metadata_json) expands
/// inside a participant's own crate, which does not depend on `const_format`,
/// so the macro cannot name that crate directly. Routing the call through this
/// crate is what makes the expansion hygienic: it resolves in the participant
/// crate no matter what is in scope there. That obligation is the only reason
/// this is public, and it is why the item cannot be made private or removed
/// while the macro exists.
#[doc(hidden)]
pub use const_format::concatcp;

/// The serialize side of the embedded metadata document.
///
/// Its variants and renames mirror
/// [`ParticipantMetadata`](crate::metadata::ParticipantMetadata) exactly - that
/// is the point: a record written through this type is, by construction, a
/// document the parser accepts.
#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(tag = "schema")]
pub enum ParticipantMetadataRecord<'a> {
    #[serde(rename = "phoxal/participant-metadata/v0")]
    V0 {
        api: RobotApi,
        schemas: ParticipantSchemas,
        id: &'a str,
        kind: ParticipantKind,
        requirement: Option<ParticipantRequirement>,
        config_schema: serde_json::Value,
    },
}

/// Const-evaluates the embedded metadata document from typed version
/// identities.
///
/// Every argument except `id` and `config_schema` is a value of a version enum;
/// the macro reads its canonical spelling through `as_str`, so no caller - and
/// in particular no proc-macro expansion - ever spells a version out. `id` is
/// the participant identity literal and `config_schema` is a `&'static str`
/// holding already-composed JSON.
///
/// Hidden from the docs: this is the ABI writer the role macros expand into,
/// not a surface a participant author calls. It is `#[macro_export]` only
/// because macro expansion in another crate needs it to be nameable.
#[doc(hidden)]
#[macro_export]
macro_rules! participant_metadata_json {
    (
        api = $api:expr,
        bus = $bus:expr,
        launch = $launch:expr,
        robot = $robot:expr,
        component = $component:expr,
        simulation = $simulation:expr,
        id = $id:expr,
        kind = $kind:expr,
        requirement = $requirement:expr,
        config_schema = $config_schema:expr $(,)?
    ) => {{
        // `concatcp!` takes constants, not method calls, so each identity
        // resolves to its canonical spelling one step earlier.
        const __PHOXAL_API: &str = $api.as_str();
        const __PHOXAL_BUS: &str = $bus.as_str();
        const __LAUNCH_ABI: &str = $launch.as_str();
        const __PHOXAL_ROBOT: &str = $robot.as_str();
        const __PHOXAL_COMPONENT: &str = $component.as_str();
        const __PHOXAL_SIMULATION: &str = $simulation.as_str();
        const __PHOXAL_KIND: &str = $kind.as_str();
        const __PHOXAL_REQUIREMENT: &str = match $requirement {
            Some(requirement) => match requirement {
                $crate::metadata::ParticipantRequirement::DifferentialDriveVelocity => {
                    "\"differential_drive_velocity\""
                }
            },
            None => "null",
        };

        $crate::emit::concatcp!(
            "{\"schema\":\"phoxal/participant-metadata/v0\",\"api\":\"",
            __PHOXAL_API,
            "\",\"schemas\":{\"bus\":\"",
            __PHOXAL_BUS,
            "\",\"launch\":\"",
            __LAUNCH_ABI,
            "\",\"robot\":\"",
            __PHOXAL_ROBOT,
            "\",\"component\":\"",
            __PHOXAL_COMPONENT,
            "\",\"simulation\":\"",
            __PHOXAL_SIMULATION,
            "\"},\"id\":\"",
            $id,
            "\",\"kind\":\"",
            __PHOXAL_KIND,
            "\",\"requirement\":",
            __PHOXAL_REQUIREMENT,
            ",\"config_schema\":",
            $config_schema,
            "}"
        )
    }};
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::metadata::ParticipantMetadata;
    use crate::version::{BusAbi, ComponentSchema, LaunchAbi, RobotSchema, SimulationSchema};

    const CONFIG_SCHEMA: &str = r#"{"type":"null"}"#;

    const EMBEDDED: &str = participant_metadata_json!(
        api = RobotApi::V0_2,
        bus = BusAbi::V0,
        launch = LaunchAbi::V0,
        robot = RobotSchema::V0,
        component = ComponentSchema::V0,
        simulation = SimulationSchema::V0,
        id = "drive",
        kind = ParticipantKind::Service,
        requirement = None,
        config_schema = CONFIG_SCHEMA,
    );

    fn typed_record() -> ParticipantMetadataRecord<'static> {
        ParticipantMetadataRecord::V0 {
            api: RobotApi::V0_2,
            schemas: ParticipantSchemas {
                bus: BusAbi::V0,
                launch: LaunchAbi::V0,
                robot: RobotSchema::V0,
                component: ComponentSchema::V0,
                simulation: SimulationSchema::V0,
            },
            id: "drive",
            kind: ParticipantKind::Service,
            requirement: None,
            config_schema: serde_json::json!({"type": "null"}),
        }
    }

    #[test]
    fn the_const_writer_emits_exactly_what_the_typed_record_serializes() {
        let const_written: serde_json::Value =
            serde_json::from_str(EMBEDDED).expect("the const writer emits a JSON document");
        let typed = serde_json::to_value(typed_record()).expect("the typed record serializes");
        assert_eq!(const_written, typed);
    }

    #[test]
    fn an_emitted_record_parses_back_into_every_typed_identity() {
        let ParticipantMetadata::V0 {
            api,
            schemas,
            id,
            kind,
            requirement,
            config_schema,
        } = ParticipantMetadata::from_bytes(EMBEDDED.as_bytes())
            .expect("the writer's own output must satisfy the parser");

        assert_eq!(api, RobotApi::V0_2);
        assert_eq!(schemas.bus, BusAbi::V0);
        assert_eq!(schemas.launch, LaunchAbi::V0);
        assert_eq!(schemas.robot, RobotSchema::V0);
        assert_eq!(schemas.component, ComponentSchema::V0);
        assert_eq!(schemas.simulation, SimulationSchema::V0);
        assert_eq!(id, "drive");
        assert_eq!(kind, ParticipantKind::Service);
        assert_eq!(requirement, None);
        assert_eq!(config_schema, serde_json::json!({"type": "null"}));
    }

    /// The embedded section is read as a whole document, so the const writer
    /// has to emit one - not a fragment a reader would have to repair.
    #[test]
    fn the_const_written_document_is_self_contained() {
        assert!(
            EMBEDDED.starts_with('{') && EMBEDDED.ends_with('}'),
            "{EMBEDDED}"
        );
        assert_eq!(EMBEDDED.len(), EMBEDDED.trim().len());
    }
}
