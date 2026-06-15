use std::borrow::Cow;
use std::time::Instant;

use anyhow::{Result, anyhow, ensure};
use phoxal::api::component::capability::{
    camera::{
        self as camera_contract,
        v1::{Encoding as CameraEncoding, Frame as CameraFrame},
    },
    depth::{self as depth_contract, v1::Depth as DepthFrame},
    profile::v1::{CameraProfileEncoding, CameraProfileSpec, DepthProfileSpec},
};
use phoxal::api::motion::{self as motion_contract, v1::ManualCommand};
use phoxal::api::topic;
use phoxal::bus::liveliness::declare_liveliness_token;
use phoxal::bus::typed::{Received, TypedTopicSubscriber};
use phoxal::runtime::RobotRuntimeArgs;
use phoxal::runtime::{ScenarioDescriptor, ScenarioKind};
use phoxal::scenario::harness::ScenarioContext;
use phoxal::scenario::webots::{command_deadline, context_from_args};

pub const SCENARIOS: &[ScenarioDescriptor] = &[ScenarioDescriptor {
    name: Cow::Borrowed("p2-stream-profile-camera-downsample"),
    summary: Cow::Borrowed("Checks requested camera/depth downsample profiles in Webots."),
    kind: ScenarioKind::Webots {
        world: Cow::Borrowed("ArenaWorld"),
    },
    phase: phoxal::runtime::Phase::P2,
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

    let camera_topic = topic::new()
        .component("front_camera")
        .camera("rgb")
        .profile(camera_profile.to_string())
        .data();
    let depth_topic = topic::new()
        .component("front_camera")
        .depth("depth")
        .profile(depth_profile.to_string())
        .data();
    let camera_profile_key = camera_topic.key().into_owned();
    let depth_key = depth_topic.key().into_owned();
    let camera_subscriber = ctx.bus().subscriber(&camera_topic).await?;
    let depth_subscriber = ctx.bus().subscriber(&depth_topic).await?;

    let _camera_token = declare_liveliness_token(ctx.bus(), &camera_profile_key)
        .await
        .map_err(|error| anyhow!(error.to_string()))?;
    let _depth_token = declare_liveliness_token(ctx.bus(), &depth_key)
        .await
        .map_err(|error| anyhow!(error.to_string()))?;

    ctx.publish_manual_command(motion_contract::ManualCommand::V1(ManualCommand {
        linear_x_mps: 0.10,
        angular_z_radps: 0.0,
    }))
    .await?;
    ctx.advance_for_secs(4.0).await?;

    let camera = with_deadline(deadline, next_camera_profile_frame(&camera_subscriber)).await?;
    ensure!(
        camera.value.width() == 320
            && camera.value.height() == 240
            && camera.value.encoding() == CameraEncoding::Rgb8,
        "requested camera profile produced {}x{} {:?}, expected 320x240 rgb8",
        camera.value.width(),
        camera.value.height(),
        camera.value.encoding()
    );

    let depth = with_deadline(deadline, next_depth_profile_frame(&depth_subscriber)).await?;
    ensure!(
        depth.value.width() == Some(320) && depth.value.height() == Some(240),
        "requested depth profile produced {:?}x{:?}, expected 320x240",
        depth.value.width(),
        depth.value.height()
    );
    ensure!(
        depth.value.samples_mm().len() == 320 * 240,
        "requested depth profile produced {} samples, expected {}",
        depth.value.samples_mm().len(),
        320 * 240
    );

    Ok(())
}

async fn next_camera_profile_frame(
    subscriber: &TypedTopicSubscriber<camera_contract::Frame>,
) -> Result<Received<CameraFrame>> {
    let Received { at_ns, value } = next_profile_frame(subscriber).await?;
    let camera_contract::Frame::V1(value) = value;
    Ok(Received { at_ns, value })
}

async fn next_depth_profile_frame(
    subscriber: &TypedTopicSubscriber<depth_contract::Depth>,
) -> Result<Received<DepthFrame>> {
    let Received { at_ns, value } = next_profile_frame(subscriber).await?;
    let depth_contract::Depth::V1(value) = value;
    Ok(Received { at_ns, value })
}

async fn next_profile_frame<T>(subscriber: &TypedTopicSubscriber<T>) -> Result<Received<T>>
where
    T: serde::de::DeserializeOwned,
{
    match subscriber.recv().await {
        Ok(value) => Ok(value),
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
