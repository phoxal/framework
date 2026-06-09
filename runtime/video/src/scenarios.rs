use std::borrow::Cow;
use std::time::Instant;

use anyhow::{Result, anyhow, ensure};
use phoxal::api::component::v1::capability::{
    camera::{Encoding as CameraEncoding, Frame as CameraFrame},
    depth::Depth as DepthFrame,
    profile::{CameraProfileEncoding, CameraProfileSpec, DepthProfileSpec},
    profile_path,
};
use phoxal::api::motion::v1::ManualCommand;
use phoxal::bus::liveliness::declare_liveliness_token;
use phoxal::bus::pubsub::Stamped;
use phoxal::runtime::RobotRuntimeArgs;
use phoxal::runtime::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::harness::ScenarioContext;
use phoxal::scenario::webots::{command_deadline, context_from_args};

pub const SCENARIOS: &[ScenarioDescriptor] = &[ScenarioDescriptor {
    name: Cow::Borrowed("p2-stream-profile-camera-downsample"),
    summary: Cow::Borrowed("Checks requested camera/depth downsample profiles in Webots."),
    kind: ScenarioKind::Webots {
        world: Cow::Borrowed("ArenaWorld"),
    },
    phase: phoxal::runtime::runtime::Phase::P2,
    timeout_secs: 120,
    category: Cow::Borrowed("stream-profile"),
    tier: 2,
}];

pub async fn run(name: &str, common: &RobotRuntimeArgs) -> Result<()> {
    match name {
        "p2-stream-profile-camera-downsample" => {
            let ctx = context_from_args(common).await?;
            ctx.reset_simulation().await?;
            assert_p2_stream_profile_camera_downsample(&ctx, deadline_for(name)?).await
        }
        _ => anyhow::bail!("video has no scenario '{name}'"),
    }
}

fn deadline_for(name: &str) -> Result<Instant> {
    let timeout_secs = SCENARIOS
        .iter()
        .find(|scenario| scenario.name.as_ref() == name)
        .map(|scenario| scenario.timeout_secs)
        .unwrap_or(60);
    command_deadline(timeout_secs)
}

async fn assert_p2_stream_profile_camera_downsample(
    ctx: &ScenarioContext,
    deadline: Instant,
) -> Result<()> {
    let camera_profile = CameraProfileSpec {
        width_px: 320,
        height_px: 240,
        publish_rate_hz: 5.0,
        encoding: CameraProfileEncoding::Rgb8,
    }
    .to_profile_id()?;
    let depth_profile = DepthProfileSpec {
        width_px: 320,
        height_px: 240,
        publish_rate_hz: 5.0,
    }
    .to_profile_id()?;

    let camera_path = profile_path("front_camera", "rgb", &camera_profile);
    let depth_path = profile_path("front_camera", "depth", &depth_profile);
    let camera_subscriber =
        phoxal::bus::pubsub::subscribe::<Stamped<CameraFrame>>(ctx.bus(), &camera_path)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;
    let depth_subscriber =
        phoxal::bus::pubsub::subscribe::<Stamped<DepthFrame>>(ctx.bus(), &depth_path)
            .await
            .map_err(|error| anyhow!(error.to_string()))?;

    let _camera_token = declare_liveliness_token(ctx.bus(), &camera_path)
        .await
        .map_err(|error| anyhow!(error.to_string()))?;
    let _depth_token = declare_liveliness_token(ctx.bus(), &depth_path)
        .await
        .map_err(|error| anyhow!(error.to_string()))?;

    ctx.publish_manual_command(ManualCommand {
        linear_x_mps: 0.10,
        angular_z_radps: 0.0,
    })
    .await?;
    ctx.advance_for_secs(4.0).await?;

    let camera = with_deadline(
        deadline,
        next_profile_frame::<CameraFrame>(&camera_subscriber),
    )
    .await?;
    ensure!(
        camera.data.width() == 320
            && camera.data.height() == 240
            && camera.data.encoding() == CameraEncoding::Rgb8,
        "requested camera profile produced {}x{} {:?}, expected 320x240 rgb8",
        camera.data.width(),
        camera.data.height(),
        camera.data.encoding()
    );

    let depth = with_deadline(
        deadline,
        next_profile_frame::<DepthFrame>(&depth_subscriber),
    )
    .await?;
    ensure!(
        depth.data.width() == Some(320) && depth.data.height() == Some(240),
        "requested depth profile produced {:?}x{:?}, expected 320x240",
        depth.data.width(),
        depth.data.height()
    );
    ensure!(
        depth.data.samples_mm().len() == 320 * 240,
        "requested depth profile produced {} samples, expected {}",
        depth.data.samples_mm().len(),
        320 * 240
    );

    Ok(())
}

async fn next_profile_frame<T>(
    subscriber: &phoxal::bus::zenoh::TypedSubscriber<Stamped<T>>,
) -> Result<Stamped<T>>
where
    T: serde::de::DeserializeOwned + phoxal::bus::zenoh::TypedSchema,
{
    match subscriber.recv_async().await {
        Ok(Ok(value)) => Ok(value),
        Ok(Err(error)) => Err(anyhow!(
            "requested profile payload failed to decode: {error}"
        )),
        Err(error) => Err(anyhow!("requested profile subscriber failed: {error}")),
    }
}

async fn with_deadline<T>(
    deadline: Instant,
    future: impl std::future::Future<Output = Result<T>>,
) -> Result<T> {
    let remaining = deadline
        .checked_duration_since(Instant::now())
        .ok_or_else(|| anyhow!("video scenario exceeded wallclock timeout"))?;
    tokio::time::timeout(remaining, future)
        .await
        .map_err(|_| anyhow!("requested profile frame exceeded wallclock timeout"))?
}
