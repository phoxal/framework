use std::collections::{HashMap, HashSet, VecDeque};

use gilrs::{Button, EventType, Gamepad, GamepadId, Gilrs};
use phoxal::api;
use phoxal::prelude::*;
use phoxal::raw::Subscriber;

const TRIGGER_DEADZONE: f32 = 0.08;

// Plan #15: a tool is a thin raw-bus runner - no `Participant::step`. The 50 Hz poll loop
// this tool needs runs as a managed task registered from `Participant::setup`, so the
// runner can cancel, join, and fault it if it exits unexpectedly.
const POLL_HZ: f64 = 50.0;
const MAX_DEVICE_REGISTRY: usize = 64;
const STOP_REPEAT_COUNT: usize = 3;
const MAX_SELECT_ID_BYTES: usize = 128;
const MAX_STABLE_ID_SUFFIX_BYTES: usize = 3;

fn truncate_utf8(value: &str, max_bytes: usize) -> &str {
    if value.len() <= max_bytes {
        return value;
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end = end.saturating_sub(1);
    }
    &value[..end]
}

// Configless (Part 3 fix, shared runner/macro default): the `#[phoxal::tool]`
// macro now defaults an omitted `config = …` to `()` for tools, so this
// starts cleanly with `PHOXAL_CONFIG` ABSENT rather than requiring `'{}'`.
// Tools stay raw-bus only (decided 2026-07-09): no declared `Api` surface,
// just `ctx.raw_bus()` and the raw handle constructors.
pub struct ToolJoypadState {
    shutdown_publisher: CommandPublisher<api::motion::ManualCommand>,
}

#[phoxal::tool(state = ToolJoypadState)]
pub struct ToolJoypad;

impl Participant for ToolJoypad {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        let (manual_drive, unavailable_reason) = match ctx
            .robot()
            .map_err(|error| error.to_string())
            .and_then(ManualDrive::from_robot)
        {
            Ok(drive) => {
                tracing::info!(
                    target: "tool_joypad",
                    wheel_base_m = drive.wheel_base_m,
                    side_speed_mps = drive.side_speed_mps,
                    "robot model loaded for manual input"
                );
                (Some(drive), None)
            }
            Err(error) => {
                tracing::warn!(target: "tool_joypad", error, "manual input unavailable for this robot model");
                (None, Some(error))
            }
        };
        let bus = ctx.raw_bus();

        let manual_publisher =
            CommandPublisher::new(bus.clone(), &api::topic::client().motion().manual())?;
        let devices_publisher =
            DiagnosticPublisher::new(bus.clone(), &api::topic::owner().joypad().devices())?;
        let select_subscriber =
            Subscriber::new(&bus, &api::topic::owner().joypad().select(), 32).await?;
        let set_enabled_subscriber =
            Subscriber::new(&bus, &api::topic::owner().joypad().set_enabled(), 32).await?;
        let rescan_subscriber =
            Subscriber::new(&bus, &api::topic::owner().joypad().rescan(), 32).await?;

        let shutdown_publisher = manual_publisher.clone();
        ctx.spawn_managed_with("joypad-poll", ManagedTaskPolicy::FaultOnExit, async move {
            run_joypad(
                manual_publisher,
                devices_publisher,
                select_subscriber,
                set_enabled_subscriber,
                rescan_subscriber,
                manual_drive,
                unavailable_reason,
            )
            .await
        });
        Ok((ToolJoypadState { shutdown_publisher }, ()))
    }

    async fn shutdown(&self, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        publish_stop_repeats(&state.shutdown_publisher, "tool shutdown").await;
        Ok(())
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
    /// Whether gilrs has a standardized mapping for the controls this tool
    /// reads. An enumerated but unmapped pad must never look usable while
    /// silently producing zero commands.
    mapped: bool,
}

/// Authoritative controller inventory, selection and manual authority, plus
/// structural unavailability and the last explicit request error (if any).
#[derive(Default)]
struct Registry {
    entries: HashMap<String, PadEntry>,
    device_order: VecDeque<String>,
    selected: Option<String>,
    enabled: bool,
    last_error: Option<String>,
    unavailable_reason: Option<String>,
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct ManualDrive {
    wheel_base_m: f64,
    side_speed_mps: f64,
}

impl ManualDrive {
    fn from_robot(robot: &phoxal::model::Robot) -> std::result::Result<Self, String> {
        let limits = robot
            .motion_limits()
            .validate()
            .map_err(|error| error.to_string())?;
        let phoxal::model::robot::KinematicConfig::Differential { wheel_base_m, .. } =
            robot.kinematic()
        else {
            return Err("manual input requires differential robot kinematics".to_string());
        };
        Self::from_parameters(limits, *wheel_base_m)
    }

    fn from_parameters(
        limits: phoxal::model::robot::MotionLimits,
        wheel_base_m: f64,
    ) -> std::result::Result<Self, String> {
        if !(wheel_base_m.is_finite() && wheel_base_m > 0.0) {
            return Err("robot.kinematic.wheel_base_m must be finite and > 0".to_string());
        }
        let side_speed_mps = limits
            .max_linear_speed_mps
            .min(limits.max_angular_speed_radps * wheel_base_m / 2.0);
        Ok(Self {
            wheel_base_m,
            side_speed_mps,
        })
    }
}

fn combine_unavailable_reasons(robot: Option<String>, backend: Option<String>) -> Option<String> {
    match (robot, backend) {
        (Some(robot), Some(backend)) => Some(format!("{robot}; {backend}")),
        (Some(reason), _) | (_, Some(reason)) => Some(reason),
        (None, None) => None,
    }
}

/// Owns the gamepad handle and bus handles for the lifetime of the tool.
/// Polls the selected pad at [`POLL_HZ`] publishing `ManualCommand`, and
/// services the `joypad::Select`/`joypad::SetEnabled`/`joypad::Rescan`
/// commands. `joypad::Devices` publishes on every device-set or selection
/// change and once per second as the tool's clockless liveness heartbeat, so a
/// late subscriber receives authoritative state without depending on robot or
/// simulation time. Runs until the runner cancels it during managed shutdown.
async fn run_joypad(
    manual_publisher: CommandPublisher<api::motion::ManualCommand>,
    devices_publisher: DiagnosticPublisher<api::joypad::Devices>,
    select_subscriber: Subscriber<api::joypad::Select>,
    set_enabled_subscriber: Subscriber<api::joypad::SetEnabled>,
    rescan_subscriber: Subscriber<api::joypad::Rescan>,
    manual_drive: Option<ManualDrive>,
    robot_unavailable: Option<String>,
) {
    let (mut gilrs, backend_unavailable) = match Gilrs::new() {
        Ok(gilrs) => (Some(gilrs), None),
        Err(error) => {
            tracing::warn!(target: "tool_joypad", error = %error, "gamepad backend unavailable; staying idle");
            (None, Some(format!("gamepad backend unavailable: {error}")))
        }
    };

    let mut registry = Registry {
        unavailable_reason: combine_unavailable_reasons(robot_unavailable, backend_unavailable),
        ..Registry::default()
    };
    if let Some(gilrs) = gilrs.as_ref() {
        rescan(gilrs, &mut registry);
    }
    log_inventory(&registry, "joypad ready");
    if publish_devices(&devices_publisher, &registry).await {
        registry.last_error = None;
    }

    let mut ticker = tokio::time::interval(std::time::Duration::from_secs_f64(1.0 / POLL_HZ));
    ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut devices_heartbeat = tokio::time::interval(std::time::Duration::from_secs(1));
    devices_heartbeat.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    // The initial state was just published above; avoid an immediate duplicate.
    devices_heartbeat.tick().await;
    let mut pending_zeros = 0_usize;
    let mut command_publish_failures = 0_u64;

    loop {
        tokio::select! {
            _ = devices_heartbeat.tick() => {
                if command_publish_failures > 0 {
                    tracing::warn!(
                        target: "tool_joypad",
                        dropped = command_publish_failures,
                        "manual command publishes failed during the last interval"
                    );
                    command_publish_failures = 0;
                }
                publish_devices_heartbeat(&devices_publisher, &registry).await;
            }
            _ = ticker.tick() => {
                publish_pending_zero(
                    &manual_publisher,
                    &mut pending_zeros,
                    &mut command_publish_failures,
                );
                let mut changed = false;
                let mut zero_required = false;
                if let Some(gilrs) = gilrs.as_mut() {
                    while let Some(event) = gilrs.next_event() {
                        gilrs.update(&event);
                        let outcome = apply_event(gilrs, &mut registry, event.id, &event.event);
                        changed |= outcome.changed;
                        zero_required |= outcome.zero_required;
                    }
                }
                if zero_required {
                    queue_stop(&mut pending_zeros);
                    publish_pending_zero(
                        &manual_publisher,
                        &mut pending_zeros,
                        &mut command_publish_failures,
                    );
                }
                if changed
                    && publish_devices(&devices_publisher, &registry).await
                {
                    registry.last_error = None;
                }

                let Some(manual_drive) = manual_drive else { continue };
                let Some(gilrs) = gilrs.as_ref() else { continue };
                let Some(selected_id) = selected_gilrs_id(&registry) else { continue };
                let gamepad = gilrs.gamepad(selected_id);
                if !gamepad.is_connected() {
                    if let Some(stable_id) = registry.selected.clone() {
                        let outcome = disconnect_stable_id(&mut registry, &stable_id);
                        if outcome.zero_required {
                            queue_stop(&mut pending_zeros);
                            publish_pending_zero(
                                &manual_publisher,
                                &mut pending_zeros,
                                &mut command_publish_failures,
                            );
                        }
                        if outcome.changed {
                            let _ = publish_devices(
                                &devices_publisher,
                                &registry,
                            )
                            .await;
                        }
                    }
                    continue;
                }
                let Some(command) = command_from_gamepad(
                    &gamepad,
                    registry.enabled,
                    manual_drive,
                ) else { continue };
                if manual_publisher.send(command).is_err() {
                    command_publish_failures = command_publish_failures.saturating_add(1);
                }
            }
            received = select_subscriber.recv() => {
                match received {
                    Ok(received) => {
                        if gilrs.is_some() {
                            tracing::info!(
                                target: "tool_joypad",
                                device_id = %truncate_utf8(&received.body.id, MAX_SELECT_ID_BYTES),
                                "controller selection requested"
                            );
                            if handle_select(&mut registry, &received.body.id) {
                                queue_stop(&mut pending_zeros);
                            }
                        } else {
                            tracing::warn!(
                                target: "tool_joypad",
                                device_id = %truncate_utf8(&received.body.id, MAX_SELECT_ID_BYTES),
                                reason = registry.unavailable_reason.as_deref().unwrap_or("controller backend unavailable"),
                                "controller selection ignored"
                            );
                        }
                        if publish_devices(&devices_publisher, &registry).await {
                            registry.last_error = None;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(target: "tool_joypad", error = %error, "select subscription failed");
                        publish_stop_repeats(&manual_publisher, "select subscription failure").await;
                        return;
                    }
                }
            }
            received = set_enabled_subscriber.recv() => {
                match received {
                    Ok(received) => {
                        tracing::info!(
                            target: "tool_joypad",
                            enabled = received.body.enabled,
                            "manual input authority change requested"
                        );
                        if handle_set_enabled(&mut registry, received.body.enabled) {
                            queue_stop(&mut pending_zeros);
                        }
                        if publish_devices(&devices_publisher, &registry).await {
                            registry.last_error = None;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(target: "tool_joypad", error = %error, "set-enabled subscription failed");
                        publish_stop_repeats(&manual_publisher, "set-enabled subscription failure").await;
                        return;
                    }
                }
            }
            received = rescan_subscriber.recv() => {
                match received {
                    Ok(_received) => {
                        let before = devices_snapshot(&registry);
                        if let Some(gilrs) = gilrs.as_ref()
                            && rescan(gilrs, &mut registry) {
                                queue_stop(&mut pending_zeros);
                            }
                        let inventory_changed = devices_snapshot(&registry) != before;
                        log_inventory(&registry, "controller rescan completed");
                        if inventory_changed
                            && publish_devices(&devices_publisher, &registry).await
                        {
                            registry.last_error = None;
                        }
                    }
                    Err(error) => {
                        tracing::warn!(target: "tool_joypad", error = %error, "rescan subscription failed");
                        publish_stop_repeats(&manual_publisher, "rescan subscription failure").await;
                        return;
                    }
                }
            }
        }
    }
}

fn queue_stop(pending_zeros: &mut usize) {
    *pending_zeros = (*pending_zeros).max(STOP_REPEAT_COUNT);
}

fn publish_pending_zero(
    publisher: &CommandPublisher<api::motion::ManualCommand>,
    pending_zeros: &mut usize,
    failures: &mut u64,
) {
    if *pending_zeros == 0 {
        return;
    }
    let zero = api::motion::ManualCommand {
        linear_x_mps: 0.0,
        angular_z_radps: 0.0,
    };
    if publisher.send(zero).is_err() {
        *failures = (*failures).saturating_add(1);
    } else {
        *pending_zeros -= 1;
    }
}

async fn publish_stop_repeats(
    publisher: &CommandPublisher<api::motion::ManualCommand>,
    reason: &'static str,
) {
    for attempt in 0..STOP_REPEAT_COUNT {
        let zero = api::motion::ManualCommand {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
        };
        if let Err(error) = publisher.send(zero) {
            tracing::warn!(
                target: "tool_joypad",
                attempt = attempt + 1,
                error = %error,
                reason,
                "failed to publish manual stop during exit"
            );
        }
        if attempt + 1 < STOP_REPEAT_COUNT {
            tokio::time::sleep(std::time::Duration::from_secs_f64(1.0 / POLL_HZ)).await;
        }
    }
}

fn has_compatible_mapping(gamepad: &Gamepad<'_>) -> bool {
    mapping_supports_manual_controls([
        gamepad.button_code(Button::LeftTrigger).is_some(),
        gamepad.button_code(Button::LeftTrigger2).is_some(),
        gamepad.button_code(Button::RightTrigger).is_some(),
        gamepad.button_code(Button::RightTrigger2).is_some(),
    ])
}

fn mapping_supports_manual_controls(controls: [bool; 4]) -> bool {
    controls.into_iter().all(std::convert::identity)
}

fn selected_gilrs_id(registry: &Registry) -> Option<GamepadId> {
    let selected = registry.selected.as_ref()?;
    registry.entries.get(selected)?.gilrs_id
}

async fn publish_devices(
    publisher: &DiagnosticPublisher<api::joypad::Devices>,
    registry: &Registry,
) -> bool {
    let devices = devices_snapshot(registry);
    if let Err(error) = publisher.publish(devices) {
        tracing::warn!(target: "tool_joypad", error = %error, "devices publish failed");
        return false;
    }
    true
}

/// Periodic state refresh for late subscribers. `last_error` is a transient
/// acknowledgement of a rejected user action, so only the event-driven publish
/// carries it; replaying it every second would turn one rejection into an
/// endless stream of apparent new errors.
async fn publish_devices_heartbeat(
    publisher: &DiagnosticPublisher<api::joypad::Devices>,
    registry: &Registry,
) {
    let devices = devices_heartbeat_snapshot(registry);
    if let Err(error) = publisher.publish(devices) {
        tracing::warn!(target: "tool_joypad", error = %error, "devices heartbeat publish failed");
    }
}

fn devices_heartbeat_snapshot(registry: &Registry) -> api::joypad::Devices {
    let mut devices = devices_snapshot(registry);
    devices.last_error = None;
    devices
}

fn devices_snapshot(registry: &Registry) -> api::joypad::Devices {
    let mut available: Vec<api::joypad::Device> = registry
        .entries
        .iter()
        .map(|(id, entry)| api::joypad::Device {
            id: id.clone(),
            name: entry.name.clone(),
            status: if !entry.connected {
                api::joypad::DeviceStatus::Disconnected
            } else if entry.mapped {
                api::joypad::DeviceStatus::Ready
            } else {
                api::joypad::DeviceStatus::Unsupported
            },
        })
        .collect();
    available.sort_by(|a, b| a.id.cmp(&b.id));
    api::joypad::Devices {
        available,
        selected: registry.selected.clone(),
        enabled: registry.enabled,
        unavailable_reason: registry.unavailable_reason.clone(),
        last_error: registry.last_error.clone(),
    }
}

fn log_inventory(registry: &Registry, message: &'static str) {
    let connected = registry
        .entries
        .values()
        .filter(|entry| entry.connected)
        .count();
    let ready = registry
        .entries
        .values()
        .filter(|entry| entry.connected && entry.mapped)
        .count();
    tracing::info!(
        target: "tool_joypad",
        known = registry.entries.len(),
        connected,
        ready,
        selected = registry.selected.as_deref(),
        enabled = registry.enabled,
        unavailable = registry.unavailable_reason.as_deref(),
        "{message}"
    );
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct RegistryChange {
    changed: bool,
    zero_required: bool,
}

/// Handle a decoded gilrs hotplug event, returning whether Devices changed and
/// whether the caller must publish an immediate zero command.
fn apply_event(
    gilrs: &Gilrs,
    registry: &mut Registry,
    id: GamepadId,
    event: &EventType,
) -> RegistryChange {
    match event {
        EventType::Connected => {
            let stable_id = observe(gilrs, registry, id);
            let (name, mapped) = registry
                .entries
                .get(&stable_id)
                .map(|entry| (entry.name.as_str(), entry.mapped))
                .unwrap_or(("unknown", false));
            tracing::info!(
                target: "tool_joypad",
                device_id = %stable_id,
                device_name = name,
                mapped,
                "controller connected"
            );
            reconcile_connected_device(registry, stable_id)
        }
        EventType::Disconnected => on_disconnected(registry, id),
        _ => RegistryChange::default(),
    }
}

fn reconcile_connected_device(registry: &mut Registry, stable_id: String) -> RegistryChange {
    let zero_required = invalidate_unready_selection(registry);
    let mapped = registry
        .entries
        .get(&stable_id)
        .is_some_and(|entry| entry.mapped);
    if registry.selected.is_none() && mapped {
        registry.selected = Some(stable_id);
        registry.enabled = false;
    }
    RegistryChange {
        changed: true,
        zero_required,
    }
}

/// Client asked to select a device by its stable wire id (`joypad::Select`).
/// Unknown/unavailable ids populate `last_error`; either way the caller
/// republishes `Devices` (that republish IS the ack).
fn handle_select(registry: &mut Registry, id: &str) -> bool {
    if id.len() > MAX_SELECT_ID_BYTES {
        let id = truncate_utf8(id, MAX_SELECT_ID_BYTES);
        let failure = format!("device id '{id}…' exceeds the {MAX_SELECT_ID_BYTES}-byte limit");
        tracing::warn!(target: "tool_joypad", device_id = id, reason = %failure, "controller selection rejected");
        registry.last_error = Some(failure);
        return false;
    }
    let selection_changes = registry.selected.as_deref() != Some(id);
    let failure = match registry.entries.get(id) {
        Some(entry) if entry.connected && entry.mapped => {
            let zero_required = registry.enabled && selection_changes;
            registry.selected = Some(id.to_string());
            if selection_changes {
                registry.enabled = false;
            }
            registry.last_error = None;
            tracing::info!(
                target: "tool_joypad",
                device_id = id,
                device_name = %entry.name,
                "controller selected"
            );
            return zero_required;
        }
        Some(entry) if entry.connected => {
            format!("device '{}' has no compatible control mapping", entry.name)
        }
        Some(_) => format!("device '{id}' is not connected"),
        None => format!("unknown device id '{id}'"),
    };
    let zero_required = !selection_changes && invalidate_selection(registry);
    tracing::warn!(target: "tool_joypad", device_id = id, reason = %failure, "controller selection rejected");
    registry.last_error = Some(failure);
    zero_required
}

/// Apply an authoritative enable/disable request. The published Devices state
/// is the acknowledgement; disabling always requests a zero command before
/// movement publication stops.
fn handle_set_enabled(registry: &mut Registry, enabled: bool) -> bool {
    if !enabled {
        let was_enabled = registry.enabled;
        registry.enabled = false;
        registry.last_error = None;
        tracing::info!(target: "tool_joypad", was_enabled, "manual input disabled");
        return was_enabled;
    }
    if let Some(reason) = registry.unavailable_reason.as_ref() {
        registry.enabled = false;
        registry.last_error = None;
        tracing::warn!(target: "tool_joypad", reason, "manual input enable rejected");
        return false;
    }
    let Some(selected) = registry.selected.as_ref() else {
        registry.enabled = false;
        let error = "no controller is selected".to_string();
        tracing::warn!(target: "tool_joypad", reason = %error, "manual input enable rejected");
        registry.last_error = Some(error);
        return false;
    };
    let ready = registry
        .entries
        .get(selected)
        .is_some_and(|entry| entry.connected && entry.mapped);
    if ready {
        registry.enabled = true;
        registry.last_error = None;
        tracing::info!(target: "tool_joypad", device_id = %selected, "manual input enabled");
    } else {
        registry.enabled = false;
        let error = format!("selected device '{selected}' is not ready");
        tracing::warn!(target: "tool_joypad", device_id = %selected, reason = %error, "manual input enable rejected");
        registry.last_error = Some(error);
    }
    false
}

/// Re-enumerate every currently connected pad, reconciling against the
/// previously known set so a still-connected pad keeps its stable id, a
/// newly seen pad is assigned one, and a pad no longer present is marked
/// disconnected (not removed - it may reconnect later).
fn rescan(gilrs: &Gilrs, registry: &mut Registry) -> bool {
    let mut seen = HashSet::new();
    for (id, _gamepad) in gilrs.gamepads() {
        seen.insert(observe(gilrs, registry, id));
    }
    let zero_required = mark_missing_devices(registry, &seen);
    let reconciliation_zero = reconcile_selection(gilrs, registry);
    zero_required || reconciliation_zero
}

fn mark_missing_devices(registry: &mut Registry, seen: &HashSet<String>) -> bool {
    for (stable_id, entry) in &mut registry.entries {
        if !seen.contains(stable_id) {
            entry.gilrs_id = None;
            entry.connected = false;
        }
    }
    let selected_disconnected = registry.selected.as_ref().is_some_and(|selected| {
        registry
            .entries
            .get(selected.as_str())
            .is_some_and(|entry| !entry.connected)
    });
    let zero_required = selected_disconnected && invalidate_selection(registry);
    prune_device_registry(registry);
    zero_required
}

/// If the current selection is no longer ready, clear it, disable manual
/// authority, and require an immediate zero. If nothing is selected, default
/// to the first compatible pad while leaving authority disabled.
fn reconcile_selection(gilrs: &Gilrs, registry: &mut Registry) -> bool {
    let zero_required = invalidate_unready_selection(registry);
    if registry.selected.is_none()
        && let Some((first_id, _)) = gilrs
            .gamepads()
            .find(|(_, gamepad)| has_compatible_mapping(gamepad))
        && let Some((stable_id, _)) = registry
            .entries
            .iter()
            .find(|(_, entry)| entry.gilrs_id == Some(first_id))
    {
        registry.selected = Some(stable_id.clone());
        registry.enabled = false;
    }
    zero_required
}

fn invalidate_unready_selection(registry: &mut Registry) -> bool {
    let selection_is_ready = registry
        .selected
        .as_ref()
        .and_then(|id| registry.entries.get(id))
        .is_some_and(|entry| entry.connected && entry.mapped);
    registry.selected.is_some() && !selection_is_ready && invalidate_selection(registry)
}

fn invalidate_selection(registry: &mut Registry) -> bool {
    if registry.selected.take().is_none() {
        return false;
    }
    let was_enabled = registry.enabled;
    registry.enabled = false;
    was_enabled
}

/// Ensure `id` is represented in `registry`, reusing a previously known
/// stable id for the same physical pad (matched by [`base_device_id`]) when
/// one exists, and return that stable id.
fn observe(gilrs: &Gilrs, registry: &mut Registry, id: GamepadId) -> String {
    let gamepad = gilrs.gamepad(id);
    let name = gamepad.name().to_string();
    let mapped = has_compatible_mapping(&gamepad);

    if let Some(stable_id) = registry
        .entries
        .iter()
        .find_map(|(stable_id, entry)| (entry.gilrs_id == Some(id)).then(|| stable_id.clone()))
    {
        let entry = registry
            .entries
            .get_mut(&stable_id)
            .expect("observed controller must still exist");
        entry.name = name;
        entry.connected = true;
        entry.mapped = mapped;
        touch_device(registry, &stable_id);
        return stable_id;
    }

    let base = base_device_id(gamepad.uuid(), gamepad.name());

    if let Some(stable_id) = registry.entries.iter().find_map(|(stable_id, entry)| {
        (entry.base_id == base && entry.gilrs_id.is_none()).then(|| stable_id.clone())
    }) {
        let entry = registry
            .entries
            .get_mut(&stable_id)
            .expect("reconnecting controller must still exist");
        entry.gilrs_id = Some(id);
        entry.connected = true;
        entry.mapped = mapped;
        entry.name = name;
        touch_device(registry, &stable_id);
        return stable_id;
    }

    let stable_id = assign_stable_id(&registry.entries, &base);
    registry.entries.insert(
        stable_id.clone(),
        PadEntry {
            base_id: base,
            gilrs_id: Some(id),
            name,
            connected: true,
            mapped,
        },
    );
    touch_device(registry, &stable_id);
    stable_id
}

fn touch_device(registry: &mut Registry, stable_id: &str) {
    registry.device_order.retain(|known| known != stable_id);
    registry.device_order.push_back(stable_id.to_string());
    prune_device_registry(registry);
}

fn prune_device_registry(registry: &mut Registry) {
    while registry.entries.len() > MAX_DEVICE_REGISTRY {
        let candidate = registry
            .device_order
            .iter()
            .position(|id| {
                registry.selected.as_deref() != Some(id.as_str())
                    && registry
                        .entries
                        .get(id)
                        .is_some_and(|entry| !entry.connected)
            })
            .or_else(|| {
                registry.device_order.iter().position(|id| {
                    registry.selected.as_deref() != Some(id.as_str())
                        && registry.entries.contains_key(id)
                })
            });
        let Some(position) = candidate else {
            break;
        };
        let stable_id = registry
            .device_order
            .remove(position)
            .expect("device eviction candidate must exist");
        if let Some(entry) = registry.entries.remove(&stable_id) {
            tracing::warn!(
                target: "tool_joypad",
                device_id = %stable_id,
                device_name = %entry.name,
                connected = entry.connected,
                "controller registry capacity reached; evicting oldest device"
            );
        }
    }
    registry
        .device_order
        .retain(|stable_id| registry.entries.contains_key(stable_id));
}

fn on_disconnected(registry: &mut Registry, id: GamepadId) -> RegistryChange {
    let stable_id = registry
        .entries
        .iter()
        .find_map(|(stable_id, entry)| (entry.gilrs_id == Some(id)).then(|| stable_id.clone()));
    let Some(stable_id) = stable_id else {
        return RegistryChange::default();
    };
    disconnect_stable_id(registry, &stable_id)
}

fn disconnect_stable_id(registry: &mut Registry, stable_id: &str) -> RegistryChange {
    let name = {
        let Some(entry) = registry.entries.get_mut(stable_id) else {
            return RegistryChange::default();
        };
        entry.gilrs_id = None;
        entry.connected = false;
        entry.name.clone()
    };
    touch_device(registry, stable_id);
    let selected_disconnected = registry.selected.as_deref() == Some(stable_id);
    let zero_required = selected_disconnected && invalidate_selection(registry);
    tracing::info!(
        target: "tool_joypad",
        device_id = stable_id,
        device_name = %name,
        selection_cleared = selected_disconnected,
        "controller disconnected"
    );
    RegistryChange {
        changed: true,
        zero_required,
    }
}

/// Derive a STABLE wire id for a pad from its identity - NOT the
/// process-local `gilrs::GamepadId`, which is reassigned on every
/// connect/restart. Prefers the hardware uuid (hex-encoded); falls back to a
/// name-derived id for backends that report an all-zero uuid.
fn base_device_id(uuid: [u8; 16], name: &str) -> String {
    if uuid.iter().all(|byte| *byte == 0) {
        let name_budget = MAX_SELECT_ID_BYTES
            .saturating_sub("name:".len())
            .saturating_sub(MAX_STABLE_ID_SUFFIX_BYTES);
        format!("name:{}", truncate_utf8(name, name_budget))
    } else {
        uuid.iter().map(|byte| format!("{byte:02x}")).collect()
    }
}

/// Disambiguate a base id against ids already present in `entries` (two
/// identical controllers report the same uuid), appending `#2`, `#3`, ...
/// until the id is unique.
///
/// Known limitation: a `base` is derived from the pad's uuid (or its name when
/// the backend reports an all-zero uuid), so ids are restart-stable for any pad
/// with a distinct uuid/name. Two *physically identical* controllers that share
/// the same uuid - or two zero-uuid pads with the same name - are separated only
/// by observation-order `#N` suffixes, which can swap across a restart or a
/// reconnect in a different order. Selecting the "wrong twin" is the only
/// consequence (both are the same model); for the current development-grade
/// robots this is acceptable. A stronger per-device identity (persisted mapping
/// or a hardware serial) is deferred until a real requirement appears.
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

/// Read the four shoulder inputs and mix them into a differential-drive
/// `ManualCommand`. See [`command_from_shoulders`] for the mixing convention.
fn command_from_gamepad(
    gamepad: &Gamepad<'_>,
    enabled: bool,
    drive: ManualDrive,
) -> Option<api::motion::ManualCommand> {
    command_from_shoulders(
        enabled,
        button_value(gamepad, Button::LeftTrigger),
        button_value(gamepad, Button::LeftTrigger2),
        button_value(gamepad, Button::RightTrigger),
        button_value(gamepad, Button::RightTrigger2),
        drive,
    )
}

fn button_value(gamepad: &Gamepad<'_>, button: Button) -> f32 {
    gamepad
        .button_data(button)
        .map(gilrs::ev::state::ButtonData::value)
        .unwrap_or(0.0)
}

/// Differential shoulder preset: L2/R2 are the left/right forward triggers;
/// L1/R1 reverse only their matching trigger. A modifier alone is zero. The
/// body twist is derived from the authored wheel base and motion limits, so a
/// normalized side value is a physical differential-side speed rather than a
/// tool-owned robot speed constant.
fn command_from_shoulders(
    enabled: bool,
    reverse_left: f32,
    forward_left: f32,
    reverse_right: f32,
    forward_right: f32,
    drive: ManualDrive,
) -> Option<api::motion::ManualCommand> {
    if !enabled {
        return None;
    }
    let left = side_input(reverse_left, forward_left) * drive.side_speed_mps;
    let right = side_input(reverse_right, forward_right) * drive.side_speed_mps;
    Some(api::motion::ManualCommand {
        linear_x_mps: (left + right) / 2.0,
        angular_z_radps: (right - left) / drive.wheel_base_m,
    })
}

fn side_input(reverse_modifier: f32, forward_trigger: f32) -> f64 {
    let magnitude = f64::from(trigger(forward_trigger));
    if reverse_modifier >= 0.5 {
        -magnitude
    } else {
        magnitude
    }
}

fn trigger(value: f32) -> f32 {
    if !value.is_finite() || value.abs() < TRIGGER_DEADZONE {
        0.0
    } else {
        value.clamp(0.0, 1.0)
    }
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
    fn non_finite_trigger_values_fail_closed() {
        assert_eq!(trigger(f32::NAN), 0.0);
        assert_eq!(trigger(f32::INFINITY), 0.0);
        assert_eq!(trigger(f32::NEG_INFINITY), 0.0);
    }

    #[test]
    fn l2_r2_drive_both_differential_sides_forward() {
        let command = command_from_shoulders(true, 0.0, 1.0, 0.0, 1.0, drive()).unwrap();
        assert_eq!(command.linear_x_mps, drive().side_speed_mps);
        assert_eq!(command.angular_z_radps, 0.0);
    }

    #[test]
    fn l1_r1_reverse_only_their_matching_triggers() {
        let command = command_from_shoulders(true, 1.0, 1.0, 1.0, 1.0, drive()).unwrap();
        assert_eq!(command.linear_x_mps, -drive().side_speed_mps);
        assert_eq!(command.angular_z_radps, 0.0);
    }

    #[test]
    fn reverse_modifiers_alone_are_zero() {
        let command = command_from_shoulders(true, 1.0, 0.0, 1.0, 0.0, drive()).unwrap();
        assert_eq!(command.linear_x_mps, 0.0);
        assert_eq!(command.angular_z_radps, 0.0);
    }

    #[test]
    fn left_and_right_trigger_inputs_are_independent() {
        let command = command_from_shoulders(true, 0.0, 1.0, 0.0, 0.0, drive()).unwrap();
        assert_eq!(command.linear_x_mps, drive().side_speed_mps / 2.0);
        assert!(command.angular_z_radps < 0.0);
        let command = command_from_shoulders(true, 0.0, 0.0, 0.0, 1.0, drive()).unwrap();
        assert_eq!(command.linear_x_mps, drive().side_speed_mps / 2.0);
        assert!(command.angular_z_radps > 0.0);
    }

    #[test]
    fn disabled_manual_input_produces_no_command() {
        assert!(command_from_shoulders(false, 0.0, 1.0, 0.0, 1.0, drive()).is_none());
    }

    #[test]
    fn side_speed_respects_authored_linear_and_angular_limits() {
        let angular_limited = ManualDrive::from_parameters(
            phoxal::model::robot::MotionLimits {
                max_linear_speed_mps: 0.6,
                max_angular_speed_radps: 2.0,
            },
            0.3,
        )
        .unwrap();
        assert_eq!(angular_limited.side_speed_mps, 0.3);

        let pivot = command_from_shoulders(true, 0.0, 1.0, 1.0, 1.0, angular_limited).unwrap();
        assert_eq!(pivot.linear_x_mps, 0.0);
        assert_eq!(pivot.angular_z_radps, -2.0);

        let linear_limited = ManualDrive::from_parameters(
            phoxal::model::robot::MotionLimits {
                max_linear_speed_mps: 0.2,
                max_angular_speed_radps: 10.0,
            },
            0.3,
        )
        .unwrap();
        assert_eq!(linear_limited.side_speed_mps, 0.2);
        let straight = command_from_shoulders(true, 0.0, 1.0, 0.0, 1.0, linear_limited).unwrap();
        assert_eq!(straight.linear_x_mps, 0.2);
        assert_eq!(straight.angular_z_radps, 0.0);
    }

    #[test]
    fn manual_drive_rejects_invalid_wheel_base() {
        let limits = phoxal::model::robot::MotionLimits {
            max_linear_speed_mps: 0.6,
            max_angular_speed_radps: 2.0,
        };
        for wheel_base_m in [0.0, f64::NAN, f64::INFINITY] {
            let error = ManualDrive::from_parameters(limits, wheel_base_m).unwrap_err();
            assert!(error.contains("wheel_base_m must be finite and > 0"));
        }
    }

    #[test]
    fn manual_drive_rejects_non_differential_robot_model() {
        let root = std::path::Path::new(concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../fixture/robot/rgbd-imu-diff-drive"
        ));
        let (mut source, components, simulations, structure) =
            phoxal::model::Robot::read_sources_from_dir(root)
                .expect("fixture robot sources should load");
        let (actuators, encoders) = match &source.robot.kinematic {
            phoxal::model::source::robot::v0::KinematicConfig::Differential {
                left_actuators,
                right_actuators,
                left_encoders,
                right_encoders,
                ..
            } => (
                left_actuators
                    .iter()
                    .chain(right_actuators)
                    .cloned()
                    .collect(),
                left_encoders
                    .iter()
                    .chain(right_encoders)
                    .cloned()
                    .collect(),
            ),
            _ => unreachable!("fixture must use differential kinematics"),
        };
        source.robot.kinematic =
            phoxal::model::source::robot::v0::KinematicConfig::Omnidirectional {
                actuators,
                encoders,
            };
        let robot =
            phoxal::model::Robot::try_from_sources(source, components, simulations, structure)
                .expect("fixture sources should canonicalize");

        let error = ManualDrive::from_robot(&robot).unwrap_err();
        assert_eq!(error, "manual input requires differential robot kinematics");
    }

    fn drive() -> ManualDrive {
        ManualDrive {
            wheel_base_m: 0.3,
            side_speed_mps: 0.3,
        }
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
    fn name_fallback_ids_leave_room_for_collision_suffixes() {
        let base = base_device_id([0_u8; 16], &"controller".repeat(64));
        assert!(base.len() <= MAX_SELECT_ID_BYTES - MAX_STABLE_ID_SUFFIX_BYTES);
        let mut entries = HashMap::new();
        entries.insert(
            base.clone(),
            PadEntry {
                base_id: base.clone(),
                gilrs_id: None,
                name: "first".to_string(),
                connected: false,
                mapped: true,
            },
        );
        let duplicate = assign_stable_id(&entries, &base);
        assert!(duplicate.len() <= MAX_SELECT_ID_BYTES);
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
                mapped: true,
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
                mapped: true,
            },
        );
        assert_eq!(assign_stable_id(&entries, "abc123"), "abc123#3");
    }

    #[test]
    fn device_registry_is_bounded_and_preserves_the_selection() {
        let mut registry = Registry {
            selected: Some("pad-0".to_string()),
            ..Registry::default()
        };
        for index in 0..(MAX_DEVICE_REGISTRY + 8) {
            let id = format!("pad-{index}");
            registry.entries.insert(
                id.clone(),
                PadEntry {
                    base_id: id.clone(),
                    gilrs_id: None,
                    name: id.clone(),
                    connected: false,
                    mapped: true,
                },
            );
            touch_device(&mut registry, &id);
        }

        assert_eq!(registry.entries.len(), MAX_DEVICE_REGISTRY);
        assert_eq!(registry.device_order.len(), MAX_DEVICE_REGISTRY);
        assert!(registry.entries.contains_key("pad-0"));
    }

    #[test]
    fn an_unmapped_device_cannot_be_selected_silently() {
        let mut registry = Registry::default();
        registry.entries.insert(
            "pad".to_string(),
            PadEntry {
                base_id: "pad".to_string(),
                gilrs_id: None,
                name: "Unknown Pad".to_string(),
                connected: true,
                mapped: false,
            },
        );

        handle_select(&mut registry, "pad");

        assert!(registry.selected.is_none());
        assert_eq!(
            registry.last_error.as_deref(),
            Some("device 'Unknown Pad' has no compatible control mapping")
        );
    }

    #[test]
    fn selected_device_disconnect_requests_an_immediate_zero() {
        let mut registry = Registry {
            selected: Some("pad".to_string()),
            enabled: true,
            ..Registry::default()
        };
        registry.entries.insert(
            "pad".to_string(),
            PadEntry {
                base_id: "pad".to_string(),
                gilrs_id: None,
                name: "Pad".to_string(),
                connected: true,
                mapped: true,
            },
        );

        let outcome = disconnect_stable_id(&mut registry, "pad");

        assert_eq!(
            outcome,
            RegistryChange {
                changed: true,
                zero_required: true,
            }
        );
        assert!(registry.selected.is_none());
        assert!(!registry.enabled);
        assert!(!registry.entries["pad"].connected);
    }

    #[test]
    fn disabled_selected_device_disconnect_does_not_publish_zero() {
        let mut registry = Registry {
            selected: Some("pad".to_string()),
            ..Registry::default()
        };
        registry.entries.insert(
            "pad".to_string(),
            PadEntry {
                base_id: "pad".to_string(),
                gilrs_id: None,
                name: "Pad".to_string(),
                connected: true,
                mapped: true,
            },
        );

        let outcome = disconnect_stable_id(&mut registry, "pad");

        assert!(outcome.changed);
        assert!(!outcome.zero_required);
        assert!(registry.selected.is_none());
        assert!(!registry.enabled);
    }

    #[test]
    fn rescan_marks_missing_selected_device_disconnected_and_disabled() {
        let mut registry = Registry {
            selected: Some("missing".to_string()),
            enabled: true,
            ..Registry::default()
        };
        registry.entries.insert(
            "missing".to_string(),
            PadEntry {
                base_id: "missing".to_string(),
                gilrs_id: None,
                name: "Missing Pad".to_string(),
                connected: true,
                mapped: true,
            },
        );

        assert!(mark_missing_devices(&mut registry, &HashSet::new()));
        assert!(registry.selected.is_none());
        assert!(!registry.enabled);
        assert!(!registry.entries["missing"].connected);
    }

    #[test]
    fn enable_disable_and_selection_are_authoritative() {
        let mut registry = Registry::default();
        registry.entries.insert(
            "pad".to_string(),
            PadEntry {
                base_id: "pad".to_string(),
                gilrs_id: None,
                name: "Pad".to_string(),
                connected: true,
                mapped: true,
            },
        );

        assert!(!handle_select(&mut registry, "pad"));
        assert!(!registry.enabled);
        assert!(!handle_set_enabled(&mut registry, true));
        assert!(registry.enabled);
        assert!(handle_set_enabled(&mut registry, false));
        assert!(!registry.enabled);
        assert!(!handle_set_enabled(&mut registry, false));
    }

    #[test]
    fn enable_without_a_selection_fails_closed() {
        let mut registry = Registry::default();

        assert!(!handle_set_enabled(&mut registry, true));
        assert!(!registry.enabled);
        assert_eq!(
            registry.last_error.as_deref(),
            Some("no controller is selected")
        );
    }

    #[test]
    fn oversized_selection_id_is_bounded_and_rejected() {
        let mut registry = Registry::default();
        let requested = "x".repeat(MAX_SELECT_ID_BYTES + 10_000);

        assert!(!handle_select(&mut registry, &requested));
        let error = registry.last_error.expect("rejection acknowledgement");
        assert!(error.contains("exceeds"));
        assert!(error.len() < MAX_SELECT_ID_BYTES + 80);
        assert!(registry.selected.is_none());
    }

    #[test]
    fn enable_with_a_non_ready_selection_fails_closed() {
        let mut registry = Registry {
            selected: Some("pad".to_string()),
            ..Registry::default()
        };
        registry.entries.insert(
            "pad".to_string(),
            PadEntry {
                base_id: "pad".to_string(),
                gilrs_id: None,
                name: "Pad".to_string(),
                connected: false,
                mapped: true,
            },
        );

        assert!(!handle_set_enabled(&mut registry, true));
        assert!(!registry.enabled);
        assert_eq!(
            registry.last_error.as_deref(),
            Some("selected device 'pad' is not ready")
        );
    }

    #[test]
    fn changing_an_enabled_selection_disables_and_requires_zero() {
        let mut registry = Registry {
            selected: Some("pad-a".to_string()),
            enabled: true,
            ..Registry::default()
        };
        for id in ["pad-a", "pad-b"] {
            registry.entries.insert(
                id.to_string(),
                PadEntry {
                    base_id: id.to_string(),
                    gilrs_id: None,
                    name: id.to_string(),
                    connected: true,
                    mapped: true,
                },
            );
        }

        assert!(handle_select(&mut registry, "pad-b"));
        assert_eq!(registry.selected.as_deref(), Some("pad-b"));
        assert!(!registry.enabled);
    }

    #[test]
    fn selected_device_becoming_unsupported_disables_and_requires_zero() {
        let mut registry = Registry {
            selected: Some("pad".to_string()),
            enabled: true,
            ..Registry::default()
        };
        registry.entries.insert(
            "pad".to_string(),
            PadEntry {
                base_id: "pad".to_string(),
                gilrs_id: None,
                name: "Unsupported Pad".to_string(),
                connected: true,
                mapped: false,
            },
        );

        let outcome = reconcile_connected_device(&mut registry, "pad".to_string());
        assert_eq!(
            outcome,
            RegistryChange {
                changed: true,
                zero_required: true,
            }
        );
        assert!(registry.selected.is_none());
        assert!(!registry.enabled);
        assert!(registry.last_error.is_none());
    }

    #[test]
    fn selecting_the_current_unsupported_device_invalidates_authority() {
        let mut registry = Registry {
            selected: Some("pad".to_string()),
            enabled: true,
            ..Registry::default()
        };
        registry.entries.insert(
            "pad".to_string(),
            PadEntry {
                base_id: "pad".to_string(),
                gilrs_id: None,
                name: "Unsupported Pad".to_string(),
                connected: true,
                mapped: false,
            },
        );

        assert!(handle_select(&mut registry, "pad"));
        assert!(registry.selected.is_none());
        assert!(!registry.enabled);
        assert_eq!(
            registry.last_error.as_deref(),
            Some("device 'Unsupported Pad' has no compatible control mapping")
        );
    }

    #[test]
    fn permanent_manual_unavailability_is_separate_from_transient_errors() {
        let mut registry = Registry {
            selected: Some("pad".to_string()),
            unavailable_reason: Some(
                "manual input requires differential robot kinematics".to_string(),
            ),
            last_error: Some("old transient error".to_string()),
            ..Registry::default()
        };

        assert!(!handle_set_enabled(&mut registry, true));
        assert!(!registry.enabled);
        assert!(registry.last_error.is_none());
        assert_eq!(
            devices_snapshot(&registry).unavailable_reason.as_deref(),
            Some("manual input requires differential robot kinematics")
        );
    }

    #[test]
    fn device_snapshot_distinguishes_ready_disconnected_and_unsupported() {
        let mut registry = Registry::default();
        for (id, connected, mapped) in [
            ("ready", true, true),
            ("disconnected", false, true),
            ("unsupported", true, false),
        ] {
            registry.entries.insert(
                id.to_string(),
                PadEntry {
                    base_id: id.to_string(),
                    gilrs_id: None,
                    name: id.to_string(),
                    connected,
                    mapped,
                },
            );
        }

        let snapshot = devices_snapshot(&registry);
        assert_eq!(
            snapshot.available[0].status,
            api::joypad::DeviceStatus::Disconnected
        );
        assert_eq!(
            snapshot.available[1].status,
            api::joypad::DeviceStatus::Ready
        );
        assert_eq!(
            snapshot.available[2].status,
            api::joypad::DeviceStatus::Unsupported
        );
    }

    #[test]
    fn heartbeat_snapshot_keeps_state_but_omits_transient_error() {
        let registry = Registry {
            selected: Some("pad".to_string()),
            enabled: true,
            last_error: Some("one rejected action".to_string()),
            ..Registry::default()
        };

        let snapshot = devices_heartbeat_snapshot(&registry);
        assert_eq!(snapshot.selected.as_deref(), Some("pad"));
        assert!(snapshot.enabled);
        assert!(snapshot.last_error.is_none());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn disconnect_zero_is_published_on_the_manual_contract() {
        let bus = phoxal::raw::Bus::open(phoxal::raw::BusConfig::in_process(
            format!("test/joypad-disconnect/{}", std::process::id()),
            "robot",
        ))
        .await
        .expect("bus should open");
        let publisher = CommandPublisher::new(bus.clone(), &api::topic::client().motion().manual())
            .expect("manual publisher should attach");
        let subscriber = Subscriber::new(&bus, &api::topic::owner().motion().manual(), 1)
            .await
            .expect("manual subscriber should attach");

        let mut pending_zeros = 0;
        let mut failures = 0;
        queue_stop(&mut pending_zeros);
        publish_pending_zero(&publisher, &mut pending_zeros, &mut failures);
        assert_eq!(failures, 0);
        let received = tokio::time::timeout(std::time::Duration::from_secs(2), subscriber.recv())
            .await
            .expect("zero command should arrive")
            .expect("zero command should decode");

        assert_eq!(received.body.linear_x_mps, 0.0);
        assert_eq!(received.body.angular_z_radps, 0.0);
        bus.close().await.expect("bus should close");
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 1)]
    async fn failed_stop_publish_keeps_the_retry_budget() {
        let bus = phoxal::raw::Bus::open(phoxal::raw::BusConfig::in_process(
            format!("test/joypad-stop-retry/{}", std::process::id()),
            "robot",
        ))
        .await
        .expect("bus should open");
        let publisher = CommandPublisher::new(bus.clone(), &api::topic::client().motion().manual())
            .expect("manual publisher should attach");
        bus.close().await.expect("bus should close");

        let mut pending_zeros = STOP_REPEAT_COUNT;
        let mut failures = 0;
        publish_pending_zero(&publisher, &mut pending_zeros, &mut failures);
        assert_eq!(pending_zeros, STOP_REPEAT_COUNT);
        assert_eq!(failures, 1);
    }

    #[test]
    fn all_manual_controls_are_required_for_compatibility() {
        assert!(!mapping_supports_manual_controls([true, true, true, false]));
        assert!(mapping_supports_manual_controls([true; 4]));
    }
}
