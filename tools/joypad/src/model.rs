use anyhow::{Context, Result};
use phoxal::api::{
    asset::{self, GetRequest, GetResponse},
    topic,
};
use phoxal::bus::Bus;
use phoxal::bus::query::Retry;
use phoxal::model::robot::RobotV1 as Robot;
use phoxal::runtime::ROBOT_FILE;

use crate::mapping::ControlScheme;

/// Fetch `robot.yaml` from the asset runtime and derive the control scheme from its kinematic kind.
/// Only the kinematic *kind* is read (a model fact); no keepalive/command semantics.
pub async fn fetch_control_scheme(bus: &Bus, retry: &Retry) -> Result<ControlScheme> {
    let robot = fetch_robot(bus, retry).await?;
    Ok(ControlScheme::from_kinematic(&robot.motion.kinematic))
}

async fn fetch_robot(bus: &Bus, retry: &Retry) -> Result<Robot> {
    let response = bus
        .request(
            &topic::new().asset().get(),
            &GetRequest::V1(asset::v1::GetRequest::new(ROBOT_FILE)),
            retry,
        )
        .await?;
    let bytes = asset_bytes(ROBOT_FILE, response)?;
    let robot =
        String::from_utf8(bytes).with_context(|| format!("{ROBOT_FILE} is not valid UTF-8"))?;
    Robot::read_from_string(&robot)
}

fn asset_bytes(path: &str, response: Option<GetResponse>) -> Result<Vec<u8>> {
    match response.with_context(|| format!("asset query '{path}' returned no response"))? {
        GetResponse::V1(response) => match response {
            asset::v1::GetResponse::Ok { bytes } => Ok(bytes),
            asset::v1::GetResponse::NotFound => {
                Err(anyhow::anyhow!("asset '{path}' was not found"))
            }
            asset::v1::GetResponse::InvalidPath(reason) => Err(anyhow::anyhow!(
                "asset '{path}' query failed: invalid path {reason:?}"
            )),
            asset::v1::GetResponse::Unavailable(reason) => Err(anyhow::anyhow!(
                "asset '{path}' query failed: unavailable {reason:?}"
            )),
            asset::v1::GetResponse::Busy => Err(anyhow::anyhow!("asset '{path}' provider is busy")),
        },
    }
}
