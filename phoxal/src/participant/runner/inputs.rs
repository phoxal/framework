//! What the strict launch resolves into before `Participant::setup` runs: the
//! participant's own config block, and the finalized bundle selected by the
//! supervisor.

use std::sync::Arc;

use crate::model::Robot;
use phoxal_bundle::{ParticipantAssets, RuntimeBundle, RuntimeParticipant};
use phoxal_runtime_contract::identity::ParticipantId;

/// Deserialize the participant's `Participant::setup` config from the selected
/// runtime participant record.
///
/// A missing compiled config is JSON `null`, not a missing value: that is what
/// lets a participant declaring `config = ()` or `config = Option<T>` launch
/// with no configuration at all, while one declaring a required struct fails
/// with serde's own `invalid type: null` rather than a bespoke error.
pub(crate) fn participant_config<C: serde::de::DeserializeOwned>(
    config: Option<&serde_json::Value>,
) -> crate::Result<C> {
    let value = config.cloned().unwrap_or(serde_json::Value::Null);
    Ok(serde_json::from_value(value)?)
}

/// The two views of one finalized bundle a participant is launched against.
///
/// One value rather than two, because a participant has both or neither: the
/// canonical model and the asset fence come out of the same load, and there is
/// no launch that binds one without the other.
pub(crate) struct ParticipantBundleInputs {
    pub(crate) robot: Arc<Robot>,
    pub(crate) assets: ParticipantAssets,
    pub(crate) participant: RuntimeParticipant,
    /// The binding was validated against the canonical robot before transport
    /// startup; setup receives only this selected component instance.
    pub(crate) component_instance: Option<String>,
}

impl ParticipantBundleInputs {
    /// Load and select the finalized bundle before the bus can be opened.
    pub(crate) fn for_launch(
        root: &std::path::Path,
        participant_id: &ParticipantId,
    ) -> crate::Result<Self> {
        let bundle = RuntimeBundle::open(root).map_err(|error| {
            anyhow::anyhow!("failed to load runtime bundle {}: {error}", root.display())
        })?;
        let inputs = bundle.participant_inputs(participant_id).map_err(|error| {
            anyhow::anyhow!(
                "participant '{participant_id}' is not present in runtime bundle {}: {error}",
                root.display()
            )
        })?;
        Ok(Self {
            robot: inputs.robot,
            assets: inputs.assets,
            component_instance: inputs
                .participant
                .binding
                .as_ref()
                .map(|binding| binding.component_instance.to_string()),
            participant: inputs.participant,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use phoxal_fixture::staged_bundle;

    /// A participant that declares no config, or an optional one, launches with
    /// a null compiled config; one that declares a required config does not.
    #[test]
    fn an_absent_config_is_json_null_not_a_missing_value() {
        #[derive(Debug, serde::Deserialize)]
        struct Required {
            #[expect(dead_code, reason = "the field exists to make the config required")]
            port: String,
        }

        participant_config::<()>(None).expect("a configless participant accepts absent config");
        assert!(
            participant_config::<Option<Required>>(None)
                .expect("an optional config accepts absent config")
                .is_none()
        );

        let error = participant_config::<Required>(None)
            .expect_err("a required config must reject absent config");
        assert!(
            format!("{error}").contains("invalid type: null"),
            "unexpected absent-config error: {error:#}"
        );

        let supplied = serde_json::json!({ "port": "/dev/ttyUSB0" });
        participant_config::<Required>(Some(&supplied)).expect("a supplied config deserializes");
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
        let error = participant_config::<RejectingConfig>(Some(&value))
            .expect_err("custom deserialization must reject before transport startup");
        assert!(format!("{error}").contains("custom config rejection"));
    }

    /// The launch bundle binds the model and assets a participant reads through
    /// `ctx.robot()` and `ctx.assets()`. No bundle means neither, which is what
    /// one value rather than two makes unrepresentable.
    #[test]
    fn the_bundle_binds_the_model_and_assets_together() {
        let bundle = staged_bundle();
        let inputs = ParticipantBundleInputs::for_launch(
            bundle.path(),
            &ParticipantId::new("drive_motor-front_left_drive").expect("test participant"),
        )
        .expect("the staged bundle loads");
        assert_eq!(inputs.robot.id().as_str(), "rgbd-imu-diff-drive");
        assert_eq!(
            inputs.component_instance.as_deref(),
            Some("front_left_drive")
        );
        assert!(
            inputs
                .assets
                .read(
                    &crate::AssetId::new("components/drive_motor/meshes/drive_motor.obj").unwrap()
                )
                .is_ok()
        );
        // `bin/` is outside the participant fence even though it is inside the
        // bundle the runner was pointed at.
        assert!(
            inputs
                .assets
                .read(&crate::AssetId::new("bin/brain").unwrap())
                .is_err()
        );

        let missing = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixture/robot");
        assert!(
            ParticipantBundleInputs::for_launch(
                &missing,
                &ParticipantId::new("drive_motor").expect("test participant"),
            )
            .is_err(),
            "a directory that is not a finalized bundle must fail the launch, not bind nothing"
        );
    }
}
