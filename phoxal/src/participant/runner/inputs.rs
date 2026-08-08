//! What a launch record resolves into before `Participant::setup` runs: the
//! participant's own config block, and the finalized bundle it was launched
//! against.

use std::sync::Arc;

use crate::model::Robot;
use phoxal_bundle::{ParticipantAssets, RuntimeBundle, RuntimeParticipant};
use phoxal_runtime_contract::identity::ParticipantId;

/// Deserialize the participant's `Participant::setup` config from the launch.
///
/// An absent `PHOXAL_CONFIG` is JSON `null`, not a missing value: that is what
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
}

impl ParticipantBundleInputs {
    /// Load the finalized bundle a launch names, if it names one. `Ok(None)` is
    /// the ordinary "launched without a bundle root" case; an `Err` means a root
    /// was named and is not a finalized bundle, which fails the launch rather
    /// than silently binding nothing.
    pub(crate) fn for_launch(
        bundle_root: Option<&std::path::Path>,
        participant_id: &str,
    ) -> crate::Result<Option<Self>> {
        let Some(root) = bundle_root else {
            return Ok(None);
        };
        let participant_id = ParticipantId::new(participant_id.to_string())
            .map_err(|error| anyhow::anyhow!("invalid launch participant id: {error}"))?;
        let bundle = RuntimeBundle::open(root).map_err(|error| {
            anyhow::anyhow!("failed to load runtime bundle {}: {error}", root.display())
        })?;
        let inputs = bundle
            .participant_inputs(&participant_id)
            .map_err(|error| {
                anyhow::anyhow!(
                    "participant '{participant_id}' is not present in runtime bundle {}: {error}",
                    root.display()
                )
            })?;
        Ok(Some(ParticipantBundleInputs {
            robot: inputs.robot,
            assets: inputs.assets,
            participant: inputs.participant,
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    use phoxal_fixture::staged_bundle;

    /// A participant that declares no config, or an optional one, launches with
    /// `PHOXAL_CONFIG` absent; one that declares a required config does not, and
    /// says why in serde's own words.
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

    /// The launch bundle binds the model and assets a participant reads through
    /// `ctx.robot()` and `ctx.assets()`. No bundle means neither, which is what
    /// one value rather than two makes unrepresentable.
    #[test]
    fn the_bundle_binds_the_model_and_assets_together() {
        assert!(
            ParticipantBundleInputs::for_launch(None, "drive")
                .expect("no root is not an error")
                .is_none()
        );

        let bundle = staged_bundle();
        let inputs = ParticipantBundleInputs::for_launch(
            Some(bundle.path()),
            "drive_motor-front_left_drive",
        )
        .expect("the staged bundle loads")
        .expect("a root binds the bundle");
        assert_eq!(inputs.robot.id().as_str(), "rgbd-imu-diff-drive");
        assert!(
            inputs
                .assets
                .path(
                    &crate::AssetId::new("components/drive_motor/meshes/drive_motor.obj").unwrap()
                )
                .is_ok()
        );
        // `bin/` is outside the participant fence even though it is inside the
        // bundle the runner was pointed at.
        assert!(
            inputs
                .assets
                .path(&crate::AssetId::new("bin/brain").unwrap())
                .is_err()
        );

        let missing = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../fixture/robot");
        assert!(
            ParticipantBundleInputs::for_launch(Some(&missing), "drive_motor").is_err(),
            "a directory that is not a finalized bundle must fail the launch, not bind nothing"
        );
    }
}
