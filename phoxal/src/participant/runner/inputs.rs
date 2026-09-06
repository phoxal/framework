//! Participant configuration selected from the supervisor-established model.

#[cfg(test)]
use crate::bundle::RuntimeBundle;
use crate::identity::ParticipantId;
use crate::model::Robot;
use crate::participant::api::Participant;
use crate::participant::metadata::ParticipantKind;
use anyhow::Context;

const BRAIN_ID: &str = "brain";

/// Open one local fixture bundle for an in-process test.
///
/// Production participants receive the model and remote asset reader from the
/// supervisor during bootstrap. This helper keeps the explicit fixture path
/// available to tests that exercise the same setup API without a supervisor.
#[cfg(test)]
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
pub(crate) fn participant_config<R: Participant>(
    robot: &Robot,
    participant_id: &ParticipantId,
) -> crate::Result<R::Config> {
    let config = match R::KIND {
        ParticipantKind::Driver => {
            let component = robot.component(participant_id.as_str()).with_context(|| {
                format!(
                    "driver participant '{participant_id}' is not a component instance of robot '{}'",
                    robot.id()
                )
            })?;
            let driver = component.instance().driver().with_context(|| {
                format!(
                    "driver participant '{participant_id}' names a component instance of robot '{}' that declares no driver block",
                    robot.id()
                )
            })?;
            let component_type = component.instance().component_type();
            anyhow::ensure!(
                R::ID == component_type.as_str(),
                "driver artifact '{}' cannot launch for component instance '{participant_id}' of type '{component_type}'",
                R::ID
            );
            driver.config()
        }
        ParticipantKind::Service => {
            anyhow::ensure!(
                participant_id.as_str() == R::ID,
                "service artifact '{}' cannot launch as participant '{participant_id}'",
                R::ID
            );
            robot
                .service(participant_id.as_str())
                .with_context(|| {
                    format!(
                        "service participant '{participant_id}' is not declared by robot '{}'",
                        robot.id()
                    )
                })?
                .config()
        }
        ParticipantKind::Brain => {
            anyhow::ensure!(
                R::ID == BRAIN_ID,
                "brain artifact '{}' does not declare the canonical '{BRAIN_ID}' identity",
                R::ID
            );
            anyhow::ensure!(
                participant_id.as_str() == BRAIN_ID,
                "brain artifact cannot launch as participant '{participant_id}'"
            );
            None
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

    use crate::participant::context::SetupContext;
    use phoxal_fixture::staged_bundle;

    #[derive(Debug, Eq, PartialEq, phoxal::Config, serde::Deserialize)]
    struct ReductionConfig {
        reduction: u64,
    }

    #[phoxal::driver(
        id = "drive_motor",
        config = Option<ReductionConfig>,
        connection = can
    )]
    struct DriveMotor;

    impl Participant for DriveMotor {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    #[phoxal::driver(id = "other_driver")]
    struct OtherDriver;

    impl Participant for OtherDriver {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    #[phoxal::service(id = "drive", config = Option<String>)]
    struct DriveService;

    impl Participant for DriveService {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    #[phoxal::service(id = "unknown-service")]
    struct UnknownService;

    impl Participant for UnknownService {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

    #[phoxal::brain]
    struct Brain;

    impl Participant for Brain {
        async fn setup(
            &self,
            _ctx: &mut SetupContext<Self>,
            _config: Self::Config,
        ) -> crate::Result<(Self::State, Self::Api)> {
            Ok(((), ()))
        }
    }

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
        let driver = participant_config::<DriveMotor>(robot, &participant("front_left_drive"))
            .expect("a driven component's authored driver config deserializes");
        assert_eq!(driver, Some(ReductionConfig { reduction: 20 }));
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
            participant_config::<DriveMotor>(robot, &participant("front_right_drive"))
                .expect("a driven component with no authored driver config reads null")
                .is_none()
        );

        // The fixture authors no service config, so an official service reads
        // JSON null - the same absent-config rule a missing key follows.
        assert!(
            participant_config::<DriveService>(robot, &participant("drive"))
                .expect("a service with no authored config reads null")
                .is_none()
        );

        // The brain never appears under `services`, so it reads null too.
        participant_config::<Brain>(robot, &participant("brain"))
            .expect("the root brain has no configuration side channel");
    }

    /// A driver is launched once per component instance, so an id the robot
    /// mounts nothing for is a launch mistake rather than a configless start.
    #[test]
    fn a_driver_launched_under_a_non_component_id_fails_locally() {
        let staged = staged_bundle();
        let bundle = open_bundle(staged.path()).expect("the staged bundle opens");
        let error =
            participant_config::<DriveMotor>(bundle.robot(), &participant("not-a-component"))
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
        let error = participant_config::<DriveMotor>(bundle.robot(), &participant("imu"))
            .expect_err("an undriven component instance runs no driver");
        assert!(
            format!("{error:#}").contains("declares no driver block"),
            "{error:#}"
        );
    }

    /// The launch identity selects one expected process descriptor. A binary
    /// whose compiled role or artifact identity disagrees with that descriptor
    /// is refused before its config can be mistaken for another participant's.
    #[test]
    fn wrong_compiled_roles_and_artifacts_fail_before_ready() {
        let staged = staged_bundle();
        let bundle = open_bundle(staged.path()).expect("the staged bundle opens");
        let robot = bundle.robot();

        for (error, expected) in [
            (
                participant_config::<DriveService>(robot, &participant("front_left_drive"))
                    .expect_err("a service artifact cannot impersonate a driver instance"),
                "service artifact 'drive'",
            ),
            (
                participant_config::<DriveMotor>(robot, &participant("drive"))
                    .expect_err("a driver artifact cannot impersonate a service"),
                "is not a component instance",
            ),
            (
                participant_config::<Brain>(robot, &participant("drive"))
                    .expect_err("the brain cannot launch under a service identity"),
                "brain artifact cannot launch",
            ),
            (
                participant_config::<OtherDriver>(robot, &participant("front_left_drive"))
                    .expect_err("a driver artifact must match the mounted component type"),
                "driver artifact 'other_driver'",
            ),
        ] {
            assert!(format!("{error:#}").contains(expected), "{error:#}");
        }
    }

    /// A configless service still has to exist in the supervisor model. Null
    /// configuration is not evidence that an unknown participant is valid.
    #[test]
    fn an_unknown_configless_service_fails_before_ready() {
        let staged = staged_bundle();
        let bundle = open_bundle(staged.path()).expect("the staged bundle opens");
        let error =
            participant_config::<UnknownService>(bundle.robot(), &participant("unknown-service"))
                .expect_err("an unknown service must not be treated as configless");
        assert!(
            format!("{error:#}").contains("is not declared"),
            "{error:#}"
        );
    }

    /// A fixture bundle binds the model and local assets a test reads through
    /// `ctx.robot()` and `ctx.assets()`, and a malformed directory refuses
    /// fixture setup instead of binding nothing.
    #[test]
    fn the_bundle_binds_the_model_and_its_assets() {
        let staged = staged_bundle();
        let bundle = open_bundle(staged.path()).expect("the staged bundle opens");
        assert_eq!(bundle.robot().id().as_str(), "rgbd-imu-diff-drive");
        assert!(
            bundle
                .assets()
                .read_local(
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
                .read_local(&crate::AssetId::new("bin/brain").expect("a canonical asset id"))
                .is_err()
        );

        let missing = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixture/robot");
        assert!(
            open_bundle(&missing).is_err(),
            "a directory that is not a bundle must fail fixture setup, not bind nothing"
        );
    }
}
