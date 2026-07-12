use std::collections::{HashMap, HashSet};

use gilrs::{Button, EventType, Gamepad, GamepadId, Gilrs};
use phoxal::prelude::*;
use phoxal::raw::{Publisher, Subscriber};
use phoxal_api::y2026_1 as motion_api;
use phoxal_api::y2026_9 as api;

const LINEAR_SCALE_MPS: f64 = 0.6;
const ANGULAR_SCALE_RADPS: f64 = 1.5;
const TRIGGER_DEADZONE: f32 = 0.08;

// Plan #15: a tool is a thin raw-bus runner - no `#[step]`. The 50 Hz poll loop
// this tool needs runs as a managed task registered from `#[setup]`, so the
// runner can cancel, join, and fault it if it exits unexpectedly.
const POLL_HZ: f64 = 50.0;

// Configless (Part 3 fix, shared runner/macro default): the `#[phoxal::tool]`
// macro now defaults an omitted `config = …` to `()` for tools, so this
// starts cleanly with `PHOXAL_CONFIG` ABSENT rather than requiring `'{}'`.
// Tools stay raw-bus only (decided 2026-07-09): no declared `Api` surface,
// just `ctx.raw_bus()` and the raw handle constructors.
#[phoxal::tool(id = "joypad")]
struct ToolJoypad;

#[phoxal::behavior]
impl ToolJoypad {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>) -> Result<(Self, Self::Api)> {
        let cap = ctx.owner_capability();
        let bus = ctx.raw_bus();

        let manual_publisher =
            Publisher::new(bus.clone(), &motion_api::topic::new().motion().manual())?;
        let devices_publisher = Publisher::new(
            bus.clone(),
            &api::topic::internal::new(cap).joypad().devices(),
        )?;
        let connect_subscriber =
            Subscriber::new(&bus, &api::topic::internal::new(cap).joypad().connect(), 32).await?;
        let rescan_subscriber =
            Subscriber::new(&bus, &api::topic::internal::new(cap).joypad().rescan(), 32).await?;

        ctx.spawn_managed_with("joypad-poll", ManagedTaskPolicy::FaultOnExit, async move {
            run_joypad(
                manual_publisher,
                devices_publisher,
                connect_subscriber,
                rescan_subscriber,
            )
            .await
        });
        Ok((Self, ()))
    }
}

/// One gamepad the poll loop is tracking, keyed by its STABLE wire id.
struct PadEntry {
    /// The identity the stable id was derived from (uuid-hex, or a
    /// name-derived fallback). Used to re-associate a reconnecting pad with
    /// its previous stable id, since `gilrs::GamepadId` is process-local and
    /// not reused across a disconnect/reconnect cycle.
    base_id: String,
    /// The CURRENT process-local gilrs id, if connected. `None` while the
    /// pad is known but not presently plugged in.
    gilrs_id: Option<GamepadId>,
    name: String,
    connected: bool,
}

/// All pads the tool has observed, plus which one is selected for the
/// `ManualCommand` poll loop and the last device-management error (if any).
#[derive(Default)]
struct Registry {
    entries: HashMap<String, PadEntry>,
    selected: Option<String>,
    last_error: Option<String>,
}

/// Owns the gamepad handle and bus handles for the lifetime of the tool.
/// Polls the selected pad at [`POLL_HZ`] publishing `ManualCommand`, and
/// services the `joypad::Connect`/`joypad::Rescan` commands, publishing
/// `joypad::Devices` on every device-set or selection change. Runs until the
/// runner cancels it during managed shutdown.
async fn run_joypad(
    manual_publisher: Publisher<motion_api::motion::ManualCommand>,
    devices_publisher: Publisher<api::joypad::Devices>,
    connect_subscriber: Subscriber<api::joypad::Connect>,
    rescan_subscriber: Subscriber<api::joypad::Rescan>,
) {
    let mut gilrs = match Gilrs::new() {
        Ok(gilrs) => Some(gilrs),
        Err(error) => {
            tracing::warn!(target: "tool_joypad", error = %error, "gamepad backend unavailable; staying idle");
            None
        }
    };

    let mut registry = Registry::default();
    if let Some(gilrs) = gilrs.as_ref() {
        rescan(gilrs, &mut registry);
    } else {
        registry.last_error = Some("gamepad backend unavailable".to_string());
    }
    publish_devices(&devices_publisher, &registry).await;

    let mut ticker = tokio::time::interval(std::time::Duration::from_secs_f64(1.0 / POLL_HZ));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);

    loop {
        tokio::select! {
            _ = ticker.tick() => {
                let mut changed = false;
                if let Some(gilrs) = gilrs.as_mut() {
                    while let Some(event) = gilrs.next_event() {
                        gilrs.update(&event);
                        changed |= apply_event(gilrs, &mut registry, event.id, &event.event);
                    }
                }
                if changed {
                    publish_devices(&devices_publisher, &registry).await;
                }

                let Some(gilrs) = gilrs.as_ref() else { continue };
                let Some(selected_id) = selected_gilrs_id(&registry) else { continue };
                let command = command_from_gamepad(&gilrs.gamepad(selected_id));
                if let Err(error) = manual_publisher.publish_at(now(), command).await {
                    tracing::warn!(target: "tool_joypad", error = %error, "publish failed");
                }
            }
            received = connect_subscriber.recv() => {
                match received {
                    Ok(received) => {
                        if gilrs.is_some() {
                            handle_connect(&mut registry, &received.body.id);
                        } else {
                            registry.last_error = Some("gamepad backend unavailable".to_string());
                        }
                        publish_devices(&devices_publisher, &registry).await;
                    }
                    Err(error) => {
                        tracing::warn!(target: "tool_joypad", error = %error, "connect subscription failed");
                    }
                }
            }
            received = rescan_subscriber.recv() => {
                match received {
                    Ok(_received) => {
                        if let Some(gilrs) = gilrs.as_ref() {
                            rescan(gilrs, &mut registry);
                        } else {
                            registry.last_error = Some("gamepad backend unavailable".to_string());
                        }
                        publish_devices(&devices_publisher, &registry).await;
                    }
                    Err(error) => {
                        tracing::warn!(target: "tool_joypad", error = %error, "rescan subscription failed");
                    }
                }
            }
        }
    }
}

fn selected_gilrs_id(registry: &Registry) -> Option<GamepadId> {
    let selected = registry.selected.as_ref()?;
    registry.entries.get(selected)?.gilrs_id
}

async fn publish_devices(publisher: &Publisher<api::joypad::Devices>, registry: &Registry) {
    let devices = devices_snapshot(registry);
    if let Err(error) = publisher.publish_at(now(), devices).await {
        tracing::warn!(target: "tool_joypad", error = %error, "devices publish failed");
    }
}

fn devices_snapshot(registry: &Registry) -> api::joypad::Devices {
    let mut available: Vec<api::joypad::Device> = registry
        .entries
        .iter()
        .map(|(id, entry)| api::joypad::Device {
            id: id.clone(),
            name: entry.name.clone(),
            connected: entry.connected,
        })
        .collect();
    available.sort_by(|a, b| a.id.cmp(&b.id));
    api::joypad::Devices {
        available,
        selected: registry.selected.clone(),
        last_error: registry.last_error.clone(),
    }
}

/// Handle a decoded gilrs hotplug event, mutating `registry` in place.
/// Returns `true` if the device set or selection changed (Devices should be
/// republished).
fn apply_event(gilrs: &Gilrs, registry: &mut Registry, id: GamepadId, event: &EventType) -> bool {
    match event {
        EventType::Connected => {
            let stable_id = observe(gilrs, registry, id);
            if registry.selected.is_none() {
                registry.selected = Some(stable_id);
                registry.last_error = None;
            }
            true
        }
        EventType::Disconnected => on_disconnected(registry, id),
        _ => false,
    }
}

/// Client asked to select a device by its stable wire id (`joypad::Connect`).
/// Unknown/unavailable ids populate `last_error`; either way the caller
/// republishes `Devices` (that republish IS the ack).
fn handle_connect(registry: &mut Registry, id: &str) {
    match registry.entries.get(id) {
        Some(entry) if entry.connected => {
            registry.selected = Some(id.to_string());
            registry.last_error = None;
        }
        Some(_) => {
            registry.last_error = Some(format!("device '{id}' is not connected"));
        }
        None => {
            registry.last_error = Some(format!("unknown device id '{id}'"));
        }
    }
}

/// Re-enumerate every currently connected pad, reconciling against the
/// previously known set so a still-connected pad keeps its stable id, a
/// newly seen pad is assigned one, and a pad no longer present is marked
/// disconnected (not removed - it may reconnect later).
fn rescan(gilrs: &Gilrs, registry: &mut Registry) {
    let mut seen: HashSet<GamepadId> = HashSet::new();
    for (id, _gamepad) in gilrs.gamepads() {
        seen.insert(id);
        observe(gilrs, registry, id);
    }
    for entry in registry.entries.values_mut() {
        if let Some(gilrs_id) = entry.gilrs_id {
            if !seen.contains(&gilrs_id) {
                entry.gilrs_id = None;
                entry.connected = false;
            }
        }
    }
    reconcile_selection(gilrs, registry);
}

/// If the current selection is no longer connected, clear it (reporting why)
/// and, if nothing is selected, default to the first connected pad (native
/// gilrs enumeration order), mirroring the tool's original default-selection
/// behavior.
fn reconcile_selection(gilrs: &Gilrs, registry: &mut Registry) {
    let selected_connected = registry
        .selected
        .as_ref()
        .and_then(|id| registry.entries.get(id))
        .map(|entry| entry.connected)
        .unwrap_or(false);
    if !selected_connected {
        if let Some(selected) = registry.selected.take() {
            registry.last_error = Some(format!("selected device '{selected}' disconnected"));
        }
    }
    if registry.selected.is_none() {
        if let Some((first_id, _)) = gilrs.gamepads().next() {
            if let Some((stable_id, _)) = registry
                .entries
                .iter()
                .find(|(_, entry)| entry.gilrs_id == Some(first_id))
            {
                registry.selected = Some(stable_id.clone());
            }
        }
    }
}

/// Ensure `id` is represented in `registry`, reusing a previously known
/// stable id for the same physical pad (matched by [`base_device_id`]) when
/// one exists, and return that stable id.
fn observe(gilrs: &Gilrs, registry: &mut Registry, id: GamepadId) -> String {
    if let Some((stable_id, _)) = registry
        .entries
        .iter()
        .find(|(_, entry)| entry.gilrs_id == Some(id))
    {
        return stable_id.clone();
    }

    let gamepad = gilrs.gamepad(id);
    let base = base_device_id(gamepad.uuid(), gamepad.name());
    let name = gamepad.name().to_string();

    if let Some((stable_id, entry)) = registry
        .entries
        .iter_mut()
        .find(|(_, entry)| entry.base_id == base && entry.gilrs_id.is_none())
    {
        entry.gilrs_id = Some(id);
        entry.connected = true;
        entry.name = name;
        return stable_id.clone();
    }

    let stable_id = assign_stable_id(&registry.entries, &base);
    registry.entries.insert(
        stable_id.clone(),
        PadEntry {
            base_id: base,
            gilrs_id: Some(id),
            name,
            connected: true,
        },
    );
    stable_id
}

fn on_disconnected(registry: &mut Registry, id: GamepadId) -> bool {
    let mut disconnected: Option<String> = None;
    for (stable_id, entry) in registry.entries.iter_mut() {
        if entry.gilrs_id == Some(id) {
            entry.gilrs_id = None;
            entry.connected = false;
            disconnected = Some(stable_id.clone());
            break;
        }
    }
    let Some(stable_id) = disconnected else {
        return false;
    };
    if registry.selected.as_deref() == Some(stable_id.as_str()) {
        registry.selected = None;
        registry.last_error = Some(format!("selected device '{stable_id}' disconnected"));
    }
    true
}

/// Derive a STABLE wire id for a pad from its identity - NOT the
/// process-local `gilrs::GamepadId`, which is reassigned on every
/// connect/restart. Prefers the hardware uuid (hex-encoded); falls back to a
/// name-derived id for backends that report an all-zero uuid.
fn base_device_id(uuid: [u8; 16], name: &str) -> String {
    if uuid.iter().all(|byte| *byte == 0) {
        format!("name:{name}")
    } else {
        uuid.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// Disambiguate a base id against ids already present in `entries` (two
/// identical controllers report the same uuid), appending `#2`, `#3`, ...
/// until the id is unique.
fn assign_stable_id(entries: &HashMap<String, PadEntry>, base: &str) -> String {
    if !entries.contains_key(base) {
        return base.to_string();
    }
    let mut suffix = 2u32;
    loop {
        let candidate = format!("{base}#{suffix}");
        if !entries.contains_key(&candidate) {
            return candidate;
        }
        suffix += 1;
    }
}

fn now() -> LogicalTime {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    LogicalTime::new(0, u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
}

/// Read the four shoulder inputs and mix them into a differential-drive
/// `ManualCommand`. See [`command_from_triggers`] for the mixing convention.
fn command_from_gamepad(gamepad: &Gamepad<'_>) -> motion_api::motion::ManualCommand {
    command_from_triggers(
        button_value(gamepad, Button::LeftTrigger),
        button_value(gamepad, Button::LeftTrigger2),
        button_value(gamepad, Button::RightTrigger),
        button_value(gamepad, Button::RightTrigger2),
    )
}

fn button_value(gamepad: &Gamepad<'_>, button: Button) -> f32 {
    gamepad
        .button_data(button)
        .map(gilrs::ev::state::ButtonData::value)
        .unwrap_or(0.0)
}

/// Trigger/tank differential mixing: L1/R1 (`Button::LeftTrigger`/
/// `RightTrigger`, the shoulder bumpers) drive the left/right side FORWARD;
/// L2/R2 (`LeftTrigger2`/`RightTrigger2`, the analog triggers) drive them
/// BACKWARD. Both read as an analog value in `[0, 1]` (a digital bumper
/// reads 0 or 1; an analog trigger reads the full range), combined per side
/// into a signed value in roughly `[-1, 1]`: `side = forward - backward`.
///
/// `linear_x_mps` is the average of the two sides. `angular_z_radps` follows
/// the contract's implicit REP-103-style convention (z-up; positive =
/// counter-clockwise = turn LEFT): easing the LEFT side (reducing its power
/// relative to the right) should curve the robot toward the weaker side, so
/// `angular = (right - left) / 2` - releasing L1/pressing L2 alone turns
/// left (positive), the intuitive tank-drive result.
fn command_from_triggers(
    forward_left: f32,
    backward_left: f32,
    forward_right: f32,
    backward_right: f32,
) -> motion_api::motion::ManualCommand {
    let left = (trigger(forward_left) - trigger(backward_left)) as f64;
    let right = (trigger(forward_right) - trigger(backward_right)) as f64;
    let linear = (left + right) / 2.0;
    let angular = (right - left) / 2.0;
    motion_api::motion::ManualCommand {
        linear_x_mps: linear * LINEAR_SCALE_MPS,
        angular_z_radps: angular * ANGULAR_SCALE_RADPS,
    }
}

fn trigger(value: f32) -> f32 {
    if value.abs() < TRIGGER_DEADZONE {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<ToolJoypad>()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn deadzone_zeroes_small_trigger_values() {
        assert_eq!(trigger(0.0), 0.0);
        assert_eq!(trigger(TRIGGER_DEADZONE / 2.0), 0.0);
    }

    #[test]
    fn trigger_clamps_to_unit_range() {
        assert_eq!(trigger(1.5), 1.0);
        assert_eq!(trigger(-1.5), 0.0);
    }

    #[test]
    fn straight_ahead_is_both_bumpers_full_no_turn() {
        let command = command_from_triggers(1.0, 0.0, 1.0, 0.0);
        assert_eq!(command.linear_x_mps, LINEAR_SCALE_MPS);
        assert_eq!(command.angular_z_radps, 0.0);
    }

    #[test]
    fn reverse_is_both_triggers_full_no_turn() {
        let command = command_from_triggers(0.0, 1.0, 0.0, 1.0);
        assert_eq!(command.linear_x_mps, -LINEAR_SCALE_MPS);
        assert_eq!(command.angular_z_radps, 0.0);
    }

    #[test]
    fn easing_the_left_bumper_turns_left_positive_angular() {
        // Right side full forward, left eased to half: the robot should
        // curve toward the weaker (left) side, i.e. turn left (positive
        // angular_z_radps per the REP-103-style convention documented on
        // `command_from_triggers`).
        let command = command_from_triggers(0.5, 0.0, 1.0, 0.0);
        assert_eq!(command.linear_x_mps, 0.75 * LINEAR_SCALE_MPS);
        assert_eq!(command.angular_z_radps, 0.25 * ANGULAR_SCALE_RADPS);
        assert!(command.angular_z_radps > 0.0);
    }

    #[test]
    fn easing_the_right_bumper_turns_right_negative_angular() {
        let command = command_from_triggers(1.0, 0.0, 0.5, 0.0);
        assert!(command.angular_z_radps < 0.0);
    }

    #[test]
    fn pivot_turn_is_one_side_forward_the_other_backward() {
        // Left full forward, right full backward: pure rotation, no net
        // translation.
        let command = command_from_triggers(1.0, 0.0, 0.0, 1.0);
        assert_eq!(command.linear_x_mps, 0.0);
        assert!(command.angular_z_radps < 0.0);
    }

    #[test]
    fn base_device_id_hex_encodes_a_nonzero_uuid() {
        let mut uuid = [0u8; 16];
        uuid[0] = 0xde;
        uuid[1] = 0xad;
        let expected = format!("dead{}", "0".repeat(28));
        assert_eq!(base_device_id(uuid, "Pad"), expected);
    }

    #[test]
    fn base_device_id_falls_back_to_name_for_zero_uuid() {
        assert_eq!(base_device_id([0u8; 16], "Generic Pad"), "name:Generic Pad");
    }

    #[test]
    fn assign_stable_id_returns_base_when_unused() {
        let entries: HashMap<String, PadEntry> = HashMap::new();
        assert_eq!(assign_stable_id(&entries, "abc123"), "abc123");
    }

    #[test]
    fn assign_stable_id_disambiguates_collisions() {
        let mut entries: HashMap<String, PadEntry> = HashMap::new();
        entries.insert(
            "abc123".to_string(),
            PadEntry {
                base_id: "abc123".to_string(),
                gilrs_id: None,
                name: "Pad".to_string(),
                connected: true,
            },
        );
        assert_eq!(assign_stable_id(&entries, "abc123"), "abc123#2");

        entries.insert(
            "abc123#2".to_string(),
            PadEntry {
                base_id: "abc123".to_string(),
                gilrs_id: None,
                name: "Pad".to_string(),
                connected: true,
            },
        );
        assert_eq!(assign_stable_id(&entries, "abc123"), "abc123#3");
    }
}
