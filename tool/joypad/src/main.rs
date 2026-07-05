use anyhow::Result;
use gilrs::{Axis, EventType, GamepadId, Gilrs};
use phoxal::prelude::*;
use phoxal::raw::Publisher;
use phoxal_api::y2026_1 as api;

const LINEAR_SCALE_MPS: f64 = 0.6;
const ANGULAR_SCALE_RADPS: f64 = 1.5;
const AXIS_DEADZONE: f32 = 0.08;

// Plan #15: `Tool` is a thin raw-bus runner - no `#[step]`. The 50 Hz poll loop
// this tool needs runs as a managed task registered from `#[setup]`, so the
// runner can cancel, join, and fault it if it exits unexpectedly.
const POLL_HZ: f64 = 50.0;

#[derive(phoxal::Tool)]
#[phoxal(
    id = "joypad",
    api = y2026_1,
    contracts(publishes(api::motion::ManualCommand))
)]
struct ToolJoypad {}

#[phoxal::behavior]
impl ToolJoypad {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let publisher = Publisher::new(ctx.raw_bus(), &api::topic::new().motion().manual())?;
        ctx.spawn_managed_with("joypad-poll", ManagedTaskPolicy::FaultOnExit, async move {
            poll_gamepad(publisher).await
        });
        Ok(Self {})
    }
}

/// Owns the gamepad handle and publisher for the lifetime of the tool, polling
/// at [`POLL_HZ`] and publishing a command each tick. Runs until the runner
/// cancels it during managed shutdown.
async fn poll_gamepad(publisher: Publisher<api::motion::ManualCommand>) {
    let mut gilrs = match Gilrs::new() {
        Ok(gilrs) => Some(gilrs),
        Err(error) => {
            tracing::warn!(target: "tool_joypad", error = %error, "gamepad backend unavailable; staying idle");
            None
        }
    };
    let mut selected = gilrs.as_mut().and_then(select_first_gamepad);
    let mut warned_idle = selected.is_none();
    if warned_idle {
        tracing::info!(target: "tool_joypad", "no gamepad connected; staying idle");
    }

    let mut ticker = tokio::time::interval(std::time::Duration::from_secs_f64(1.0 / POLL_HZ));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        ticker.tick().await;

        let Some(gilrs) = gilrs.as_mut() else {
            continue;
        };

        while let Some(event) = gilrs.next_event() {
            gilrs.update(&event);
            match event.event {
                EventType::Connected => {
                    if selected.is_none() {
                        selected = Some(event.id);
                        warned_idle = false;
                        tracing::info!(target: "tool_joypad", gamepad = ?event.id, "gamepad selected");
                    }
                }
                EventType::Disconnected if selected == Some(event.id) => {
                    selected = select_first_gamepad(gilrs);
                    if selected.is_none() && !warned_idle {
                        warned_idle = true;
                        tracing::info!(target: "tool_joypad", "selected gamepad disconnected; staying idle");
                    }
                }
                _ => {}
            }
        }

        if selected.is_none() {
            selected = select_first_gamepad(gilrs);
        }

        let Some(selected_id) = selected else {
            if !warned_idle {
                warned_idle = true;
                tracing::info!(target: "tool_joypad", "no gamepad connected; staying idle");
            }
            continue;
        };

        let command = command_from_gamepad(&gilrs.gamepad(selected_id));
        if let Err(error) = publisher.publish_at(now(), command).await {
            tracing::warn!(target: "tool_joypad", error = %error, "publish failed");
        }
    }
}

fn now() -> LogicalTime {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    LogicalTime::new(0, u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
}

fn select_first_gamepad(gilrs: &mut Gilrs) -> Option<GamepadId> {
    gilrs
        .gamepads()
        .find(|(_id, gamepad)| gamepad.is_connected())
        .map(|(id, _gamepad)| id)
}

fn command_from_gamepad(gamepad: &gilrs::Gamepad<'_>) -> api::motion::ManualCommand {
    let linear = -axis(gamepad.value(Axis::LeftStickY)) as f64 * LINEAR_SCALE_MPS;
    let right_yaw = axis(gamepad.value(Axis::RightStickX));
    let left_yaw = axis(gamepad.value(Axis::LeftStickX));
    let angular_axis = if right_yaw.abs() > 0.0 {
        right_yaw
    } else {
        left_yaw
    };
    api::motion::ManualCommand {
        linear_x_mps: linear,
        angular_z_radps: angular_axis as f64 * ANGULAR_SCALE_RADPS,
    }
}

fn axis(value: f32) -> f32 {
    if value.abs() < AXIS_DEADZONE {
        0.0
    } else {
        value.clamp(-1.0, 1.0)
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<ToolJoypad>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadzone_zeroes_small_axis_values() {
        assert_eq!(axis(0.0), 0.0);
        assert_eq!(axis(AXIS_DEADZONE / 2.0), 0.0);
        assert_eq!(axis(-AXIS_DEADZONE / 2.0), 0.0);
    }

    #[test]
    fn axis_clamps_large_values() {
        assert_eq!(axis(1.5), 1.0);
        assert_eq!(axis(-1.5), -1.0);
    }
}
