//! What the strict launch resolves into before `Participant::setup` runs: the
//! opened bundle, and the participant's own configuration read out of it.

use crate::bundle::RuntimeBundle;
use crate::identity::ParticipantId;
use crate::model::Robot;
use crate::participant::metadata::ParticipantKind;
use anyhow::Context;

/// Open the bundle the launch points at.
///
/// There is no selection step: the manifest is the robot model plus, for those
/// that have one, each participant's own configuration, so a participant the
/// manifest never mentions opens the bundle exactly like one it does.
pub(crate) fn open_bundle(root: &std::path::Path) -> crate::Result<RuntimeBundle> {
    RuntimeBundle::open(root)
        .with_context(|| format!("failed to open the runtime bundle at {}", root.display()))
}

/// Read this participant's own configuration out of the compiled robot.
///
/// Which entry applies is fixed by the role the binary was authored with, not
/// by searching both maps: a driver's participant id *is* a component instance
/// id and its config is the `config` half of that instance's `driver` block,
/// while a service's (and the brain's) id keys the `services` map. The
/// `connection` half is the framework's and never reaches a participant's
/// `Config`; a driver reads it through `ctx.connection()`.
///
/// A driver launched under an id the robot mounts no component for - or under
/// one it mounts an *undriven* component for - is a launch mistake, so it fails
/// here rather than starting against an empty configuration.
///
/// A missing config is JSON `null`, not a missing value: that is what lets a
/// participant declaring `config = ()` or `config = Option<T>` launch with no
/// configuration at all, while one declaring a required struct fails with
/// serde's own `invalid type: null` rather than a bespoke error.
pub(crate) fn participant_config<C: serde::de::DeserializeOwned>(
    robot: &Robot,
    participant_id: &ParticipantId,
    kind: ParticipantKind,
) -> crate::Result<C> {
    let config = match kind {
        ParticipantKind::Driver => driver_block(robot, participant_id)?.config(),
        ParticipantKind::Service | ParticipantKind::Brain => {
            robot.service_config(participant_id.as_str())
        }
    };
    deserialize_config(config)
}

/// The driver block a launched driver is named after.
pub(crate) fn driver_block<'a>(
    robot: &'a Robot,
    participant_id: &ParticipantId,
) -> crate::Result<&'a crate::model::robot::Driver> {
    robot
        .component(participant_id.as_str())
        .with_context(|| {
            format!(
                "driver '{participant_id}' is not a component instance of robot '{}'",
                robot.id()
            )
        })?
        .instance()
        .driver()
        .with_context(|| {
            format!(
                "driver '{participant_id}' names a component instance of robot '{}' that declares \
                 no driver block",
                robot.id()
            )
        })
}

/// Deserialize one already-selected config block.
pub(crate) fn deserialize_config<C: serde::de::DeserializeOwned>(
    config: Option<&serde_json::Value>,
) -> crate::Result<C> {
    let value = config.cloned().unwrap_or(serde_json::Value::Null);
    Ok(serde_json::from_value(value)?)
}

#[cfg(test)]
mod tests {
    use super::*;

    use phoxal_fixture::staged_bundle;

    fn participant(id: &str) -> ParticipantId {
        ParticipantId::new(id).expect("a test participant id")
    }

    /// A participant that declares no config, or an optional one, launches with
    /// a null compiled config; one that declares a required config does not.
    #[test]
    fn an_absent_config_is_json_null_not_a_missing_value() {
        #[derive(Debug, serde::Deserialize)]
        struct Required {
            #[expect(dead_code, reason = "the field exists to make the config required")]
            port: String,
        }

        deserialize_config::<()>(None).expect("a configless participant accepts absent config");
        assert!(
            deserialize_config::<Option<Required>>(None)
                .expect("an optional config accepts absent config")
                .is_none()
        );

        let error = deserialize_config::<Required>(None)
            .expect_err("a required config must reject absent config");
        assert!(
            format!("{error}").contains("invalid type: null"),
            "unexpected absent-config error: {error:#}"
        );

        let supplied = serde_json::json!({ "port": "/dev/ttyUSB0" });
        deserialize_config::<Required>(Some(&supplied)).expect("a supplied config deserializes");
    }

    /// Runtime JSON Schema validation is not a substitute for the binary's
    /// `Deserialize` implementation. The runner performs this typed step
    /// before opening `BusOwner`, so custom rejection cannot become a
    /// transport-visible startup failure.
    #[test]
    fn custom_config_deserialization_rejection_is_reported_locally() {
        #[derive(Debug)]
        struct RejectingConfig;

        impl<'de> serde::Deserialize<'de> for RejectingConfig {
            fn deserialize<D: serde::Deserializer<'de>>(
                _deserializer: D,
            ) -> std::result::Result<Self, D::Error> {
                Err(serde::de::Error::custom("custom config rejection"))
            }
        }

        let value = serde_json::json!({ "accepted_by_schema": true });
        let error = deserialize_config::<RejectingConfig>(Some(&value))
            .expect_err("custom deserialization must reject before transport startup");
        assert!(format!("{error}").contains("custom config rejection"));
    }

    /// The role decides which half of the manifest a participant's config comes
    /// from: a driver reads the `config` half of the driver block on the
    /// component instance it is named after, a service reads
    /// `services.<id>.config`.
    #[test]
    fn the_role_selects_which_manifest_entry_carries_the_config() {
        let staged = staged_bundle();
        let bundle = open_bundle(staged.path()).expect("the staged bundle opens");
        let robot = bundle.robot();

        // A driver reads the `config` half and only that half: the connection
        // beside it is the framework's, and never reaches the participant's own
        // `Config` where a driver could mistake it for authored settings.
        let driver = participant_config::<serde_json::Value>(
            robot,
            &participant("front_left_drive"),
            ParticipantKind::Driver,
        )
        .expect("a driven component's authored driver config deserializes");
        assert_eq!(driver, serde_json::json!({"reduction": 20}), "{driver}");
        assert_eq!(
            driver_block(robot, &participant("front_left_drive"))
                .expect("the fixture mounts a driven component")
                .connection()
                .kind(),
            crate::model::connection::ConnectionKind::Can
        );

        // A driven instance that authors no `config` reads null, the same
        // absent-config rule a missing service key follows.
        assert!(
            participant_config::<serde_json::Value>(
                robot,
                &participant("front_right_drive"),
                ParticipantKind::Driver,
            )
            .expect("a driven component with no authored driver config reads null")
            .is_null()
        );

        // The fixture authors no service config, so an official service reads
        // JSON null - the same absent-config rule a missing key follows.
        assert!(
            participant_config::<serde_json::Value>(
                robot,
                &participant("drive"),
                ParticipantKind::Service,
            )
            .expect("a service with no authored config reads null")
            .is_null()
        );

        // The brain never appears under `services`, so it reads null too.
        participant_config::<()>(robot, &participant("brain"), ParticipantKind::Brain)
            .expect("the root brain has no configuration side channel");
    }

    /// A driver is launched once per component instance, so an id the robot
    /// mounts nothing for is a launch mistake rather than a configless start.
    #[test]
    fn a_driver_launched_under_a_non_component_id_fails_locally() {
        let staged = staged_bundle();
        let bundle = open_bundle(staged.path()).expect("the staged bundle opens");
        let error = participant_config::<serde_json::Value>(
            bundle.robot(),
            &participant("not-a-component"),
            ParticipantKind::Driver,
        )
        .expect_err("a driver must name a component instance");
        assert!(
            format!("{error:#}").contains("is not a component instance"),
            "{error:#}"
        );
    }

    /// An instance the robot mounts but does not drive launches no driver, so a
    /// driver started under its id is the same class of launch mistake: it
    /// fails here rather than against an empty configuration.
    #[test]
    fn a_driver_launched_for_an_undriven_instance_fails_locally() {
        let staged = staged_bundle();
        let bundle = open_bundle(staged.path()).expect("the staged bundle opens");
        let error = participant_config::<serde_json::Value>(
            bundle.robot(),
            &participant("imu"),
            ParticipantKind::Driver,
        )
        .expect_err("an undriven component instance runs no driver");
        assert!(
            format!("{error:#}").contains("declares no driver block"),
            "{error:#}"
        );
    }

    /// The bundle binds the model and the assets a participant reads through
    /// `ctx.robot()` and `ctx.assets()`, and a directory that is not a bundle
    /// fails the launch instead of binding nothing.
    #[test]
    fn the_bundle_binds_the_model_and_its_assets() {
        let staged = staged_bundle();
        let bundle = open_bundle(staged.path()).expect("the staged bundle opens");
        assert_eq!(bundle.robot().id().as_str(), "rgbd-imu-diff-drive");
        assert!(
            bundle
                .assets()
                .read(
                    &crate::AssetId::new("components/drive_motor/meshes/drive_motor.obj")
                        .expect("a canonical asset id")
                )
                .is_ok()
        );
        // `bin/` is outside the asset fence even though it is inside the bundle
        // the runner was pointed at.
        assert!(
            bundle
                .assets()
                .read(&crate::AssetId::new("bin/brain").expect("a canonical asset id"))
                .is_err()
        );

        let missing = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixture/robot");
        assert!(
            open_bundle(&missing).is_err(),
            "a directory that is not a bundle must fail the launch, not bind nothing"
        );
    }
}
