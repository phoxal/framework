use anyhow::{Result, anyhow};
use gilrs::{Axis as GilAxis, Button as GilButton, Event, EventType, Gamepad, GamepadId, Gilrs};

use crate::mapping::GamepadState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedDevice {
    uuid: [u8; 16],
    pub name: String,
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
}

impl SelectedDevice {
    pub fn uuid_hyphenated(&self) -> String {
        format_uuid(self.uuid)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeviceRecord {
    pub id: GamepadId,
    pub name: String,
    pub uuid: [u8; 16],
    pub vendor_id: Option<u16>,
    pub product_id: Option<u16>,
}

impl DeviceRecord {
    pub fn selector(&self) -> SelectedDevice {
        SelectedDevice {
            uuid: self.uuid,
            name: self.name.clone(),
            vendor_id: self.vendor_id,
            product_id: self.product_id,
        }
    }

    pub fn uuid_hyphenated(&self) -> String {
        format_uuid(self.uuid)
    }

    pub fn matches_controller_id(&self, controller_id: &str) -> bool {
        self.uuid_hyphenated().eq_ignore_ascii_case(controller_id)
    }
}

pub enum Sample {
    Connected(GamepadState),
    Unavailable,
}

pub struct Backend {
    gilrs: Gilrs,
    active_gamepad: Option<GamepadId>,
}

impl Backend {
    pub fn new() -> Result<Self> {
        let gilrs = Gilrs::new().map_err(|error| anyhow!("failed to initialize gilrs: {error}"))?;
        Ok(Self {
            gilrs,
            active_gamepad: None,
        })
    }

    pub fn available_devices(&mut self) -> Vec<DeviceRecord> {
        self.drain_events();
        self.connected_devices()
    }

    pub fn sample_selected(&mut self, selector: &SelectedDevice) -> Sample {
        self.drain_events();

        if let Some(id) = self.active_gamepad {
            if let Some(gamepad) = self.gilrs.connected_gamepad(id)
                && match_selected_device(selector, &device_record(id, &gamepad))
            {
                return Sample::Connected(read_state(&gamepad));
            }
            self.active_gamepad = None;
        }

        let devices = self.connected_devices();
        if let Some(device) = find_selected_device(selector, &devices) {
            self.active_gamepad = Some(device.id);
            if let Some(gamepad) = self.gilrs.connected_gamepad(device.id) {
                return Sample::Connected(read_state(&gamepad));
            }
        }

        Sample::Unavailable
    }

    fn drain_events(&mut self) {
        while let Some(Event { id, event, .. }) = self.gilrs.next_event() {
            if self.active_gamepad == Some(id) && event == EventType::Disconnected {
                self.active_gamepad = None;
            }
        }
    }

    fn connected_devices(&self) -> Vec<DeviceRecord> {
        self.gilrs
            .gamepads()
            .map(|(id, gamepad)| device_record(id, &gamepad))
            .collect()
    }
}

fn device_record(id: GamepadId, gamepad: &Gamepad<'_>) -> DeviceRecord {
    DeviceRecord {
        id,
        name: gamepad.name().to_string(),
        uuid: gamepad.uuid(),
        vendor_id: gamepad.vendor_id(),
        product_id: gamepad.product_id(),
    }
}

pub fn find_selected_device(
    selector: &SelectedDevice,
    devices: &[DeviceRecord],
) -> Option<DeviceRecord> {
    devices
        .iter()
        .find(|device| match_selected_device(selector, device))
        .cloned()
}

fn match_selected_device(selector: &SelectedDevice, device: &DeviceRecord) -> bool {
    selector.uuid == device.uuid
}

fn read_state(gamepad: &Gamepad<'_>) -> GamepadState {
    let left_trigger = trigger_value(gamepad, GilAxis::LeftZ, GilButton::LeftTrigger2);
    let right_trigger = trigger_value(gamepad, GilAxis::RightZ, GilButton::RightTrigger2);

    GamepadState {
        left_stick_x: gamepad.value(GilAxis::LeftStickX),
        left_stick_y: gamepad.value(GilAxis::LeftStickY),
        right_stick_x: gamepad.value(GilAxis::RightStickX),
        left_trigger,
        right_trigger,
        l1: gamepad.is_pressed(GilButton::LeftTrigger),
        r1: gamepad.is_pressed(GilButton::RightTrigger),
        emergency_stop: gamepad.is_pressed(GilButton::Start),
    }
}

fn trigger_value(gamepad: &Gamepad<'_>, axis: GilAxis, fallback_button: GilButton) -> f32 {
    normalized_trigger_value(
        gamepad.value(axis),
        gamepad
            .button_data(fallback_button)
            .map(|data| data.value()),
    )
}

fn normalized_trigger_value(axis_value: f32, fallback_button_value: Option<f32>) -> f32 {
    let axis_value = axis_value.clamp(0.0, 1.0);
    if axis_value > 0.0 {
        return axis_value;
    }

    fallback_button_value
        .map(|value| value.clamp(0.0, 1.0))
        .unwrap_or(0.0)
}

fn format_uuid(uuid: [u8; 16]) -> String {
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        uuid[0],
        uuid[1],
        uuid[2],
        uuid[3],
        uuid[4],
        uuid[5],
        uuid[6],
        uuid[7],
        uuid[8],
        uuid[9],
        uuid[10],
        uuid[11],
        uuid[12],
        uuid[13],
        uuid[14],
        uuid[15]
    )
}

#[cfg(test)]
mod tests {
    use super::{DeviceRecord, SelectedDevice, find_selected_device, normalized_trigger_value};
    use gilrs::GamepadId;
    use std::mem::transmute;

    fn fake_gamepad_id(value: usize) -> GamepadId {
        // Test-only helper: gilrs keeps GamepadId opaque, but selector matching only
        // needs stable distinct identities in unit tests.
        unsafe { transmute::<usize, GamepadId>(value) }
    }

    #[test]
    fn selected_identity_matches_the_intended_device_record() {
        let devices = vec![
            DeviceRecord {
                id: fake_gamepad_id(1),
                name: "Controller A".to_string(),
                uuid: [1; 16],
                vendor_id: Some(0x1111),
                product_id: Some(0x2222),
            },
            DeviceRecord {
                id: fake_gamepad_id(2),
                name: "Controller B".to_string(),
                uuid: [2; 16],
                vendor_id: Some(0x3333),
                product_id: Some(0x4444),
            },
        ];

        let selected = find_selected_device(
            &SelectedDevice {
                uuid: [2; 16],
                name: "Controller B".to_string(),
                vendor_id: Some(0x3333),
                product_id: Some(0x4444),
            },
            &devices,
        )
        .expect("matching device");

        assert_eq!(selected.uuid, [2; 16]);
    }

    #[test]
    fn reconnect_lookup_prefers_uuid_identity() {
        let devices = vec![
            DeviceRecord {
                id: fake_gamepad_id(1),
                name: "Shared Name".to_string(),
                uuid: [1; 16],
                vendor_id: Some(0x1111),
                product_id: Some(0x2222),
            },
            DeviceRecord {
                id: fake_gamepad_id(2),
                name: "Shared Name".to_string(),
                uuid: [9; 16],
                vendor_id: Some(0x1111),
                product_id: Some(0x2222),
            },
        ];

        let selected = find_selected_device(
            &SelectedDevice {
                uuid: [9; 16],
                name: "Shared Name".to_string(),
                vendor_id: Some(0x1111),
                product_id: Some(0x2222),
            },
            &devices,
        )
        .expect("matching device");

        assert_eq!(selected.uuid, [9; 16]);
    }

    #[test]
    fn reconnect_lookup_does_not_fall_back_to_name_and_usb_ids() {
        let devices = vec![DeviceRecord {
            id: fake_gamepad_id(1),
            name: "Shared Name".to_string(),
            uuid: [1; 16],
            vendor_id: Some(0x1111),
            product_id: Some(0x2222),
        }];

        let selected = find_selected_device(
            &SelectedDevice {
                uuid: [9; 16],
                name: "Shared Name".to_string(),
                vendor_id: Some(0x1111),
                product_id: Some(0x2222),
            },
            &devices,
        );

        assert!(selected.is_none());
    }

    #[test]
    fn controller_id_match_is_case_insensitive_uuid_compare() {
        let device = DeviceRecord {
            id: fake_gamepad_id(1),
            name: "Controller A".to_string(),
            uuid: [
                0x2d, 0x93, 0x15, 0x10, 0xd9, 0x9f, 0x49, 0x4a, 0x8c, 0x67, 0x87, 0xfe, 0xb0, 0x5e,
                0x15, 0x94,
            ],
            vendor_id: None,
            product_id: None,
        };

        assert!(device.matches_controller_id("2D931510-D99F-494A-8C67-87FEB05E1594"));
    }

    #[test]
    fn trigger_value_falls_back_to_analog_button_when_axis_is_idle() {
        assert_eq!(normalized_trigger_value(0.0, Some(0.75)), 0.75);
    }

    #[test]
    fn trigger_value_prefers_axis_when_available() {
        assert_eq!(normalized_trigger_value(0.5, Some(0.75)), 0.5);
    }
}
