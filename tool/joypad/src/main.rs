use anyhow::Result;
use gilrs::{Axis, EventType, GamepadId, Gilrs};
use phoxal::prelude::*;
use phoxal::raw::Publisher;
use phoxal_api::y2026_1 as api;

const LINEAR_SCALE_MPS: f64 = 0.6;
const ANGULAR_SCALE_RADPS: f64 = 1.5;
const AXIS_DEADZONE: f32 = 0.08;

#[derive(phoxal::Tool)]
#[phoxal(
    id = "joypad",
    api = y2026_1,
    contracts(publishes(api::motion::ManualCommand))
)]
struct ToolJoypad {
    gilrs: Option<Gilrs>,
    selected: Option<GamepadId>,
    publisher: Publisher<api::motion::ManualCommand>,
    warned_idle: bool,
}

#[phoxal::behavior]
impl ToolJoypad {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<Self> {
        let publisher = Publisher::new(ctx.raw_bus(), &api::topic::new().motion().manual())?;

        let mut gilrs = match Gilrs::new() {
            Ok(gilrs) => Some(gilrs),
            Err(error) => {
                tracing::warn!(target: "tool_joypad", error = %error, "gamepad backend unavailable; staying idle");
                None
            }
        };
        let selected = gilrs.as_mut().and_then(select_first_gamepad);
        if selected.is_none() {
            tracing::info!(target: "tool_joypad", "no gamepad connected; staying idle");
        }

        Ok(Self {
            gilrs,
            selected,
            publisher,
            warned_idle: selected.is_none(),
        })
    }

    #[step(hz = 50)]
    async fn step(&mut self, step: StepContext) -> Result<()> {
        let Some(gilrs) = self.gilrs.as_mut() else {
            return Ok(());
        };

        while let Some(event) = gilrs.next_event() {
            gilrs.update(&event);
            match event.event {
                EventType::Connected => {
                    if self.selected.is_none() {
                        self.selected = Some(event.id);
                        self.warned_idle = false;
                        tracing::info!(target: "tool_joypad", gamepad = ?event.id, "gamepad selected");
                    }
                }
                EventType::Disconnected if self.selected == Some(event.id) => {
                    self.selected = select_first_gamepad(gilrs);
                    if self.selected.is_none() && !self.warned_idle {
                        self.warned_idle = true;
                        tracing::info!(target: "tool_joypad", "selected gamepad disconnected; staying idle");
                    }
                }
                _ => {}
            }
        }

        if self.selected.is_none() {
            self.selected = select_first_gamepad(gilrs);
        }

        let Some(selected) = self.selected else {
            if !self.warned_idle {
                self.warned_idle = true;
                tracing::info!(target: "tool_joypad", "no gamepad connected; staying idle");
            }
            return Ok(());
        };

        let command = command_from_gamepad(&gilrs.gamepad(selected));
        self.publisher.publish_at(step.time(), command).await?;
        Ok(())
    }
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
