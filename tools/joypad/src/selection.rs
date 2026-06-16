use std::io::{self, Write};
use std::time::Duration;

use anyhow::{Context, Result};
use tokio::time::sleep;
use tracing::info;

use crate::args::ControllerSelectionMode;
use crate::backend::{Backend, DeviceRecord, SelectedDevice};

const WAIT_PERIOD: Duration = Duration::from_millis(500);

pub async fn select_device(
    mut backend: Backend,
    selection: ControllerSelectionMode,
) -> Result<SelectedDevice> {
    let mut logged_missing_any_controller = false;
    let mut logged_missing_requested_controller = false;

    loop {
        let devices = backend.available_devices();
        if devices.is_empty() {
            if !logged_missing_any_controller {
                info!("Waiting for a controller to become available");
                logged_missing_any_controller = true;
            }
            logged_missing_requested_controller = false;
            sleep(WAIT_PERIOD).await;
            continue;
        }
        logged_missing_any_controller = false;

        let selected = match &selection {
            ControllerSelectionMode::Interactive => {
                print_devices(&devices);
                prompt_for_device(&devices)?
            }
            ControllerSelectionMode::Auto => devices[0].selector(),
            ControllerSelectionMode::Id(id) => {
                if let Some(selected) = devices
                    .iter()
                    .find(|device| device.matches_controller_id(id))
                    .map(DeviceRecord::selector)
                {
                    selected
                } else {
                    if !logged_missing_requested_controller {
                        info!(
                            requested_controller = %id,
                            "Waiting for requested controller to become available"
                        );
                        logged_missing_requested_controller = true;
                    }
                    sleep(WAIT_PERIOD).await;
                    continue;
                }
            }
        };
        info!(
            device_name = %selected.name,
            device_uuid = %selected.uuid_hyphenated(),
            selection_mode = %selection_mode_name(&selection),
            "Selected controller"
        );
        return Ok(selected);
    }
}

fn selection_mode_name(selection: &ControllerSelectionMode) -> &str {
    match selection {
        ControllerSelectionMode::Interactive => "interactive",
        ControllerSelectionMode::Auto => "auto",
        ControllerSelectionMode::Id(_) => "id",
    }
}

fn print_devices(devices: &[DeviceRecord]) {
    println!("Available controllers:");
    for (index, device) in devices.iter().enumerate() {
        let vendor = device
            .vendor_id
            .map(|value| format!("0x{value:04x}"))
            .unwrap_or_else(|| "n/a".to_string());
        let product = device
            .product_id
            .map(|value| format!("0x{value:04x}"))
            .unwrap_or_else(|| "n/a".to_string());
        println!(
            "  {}. {} (uuid={}, vendor={}, product={})",
            index + 1,
            device.name,
            device.uuid_hyphenated(),
            vendor,
            product
        );
    }
}

fn prompt_for_device(devices: &[DeviceRecord]) -> Result<SelectedDevice> {
    loop {
        print!("Select controller [1-{}]: ", devices.len());
        io::stdout()
            .flush()
            .context("failed to flush controller prompt")?;

        let mut line = String::new();
        io::stdin()
            .read_line(&mut line)
            .context("failed to read controller selection")?;

        let Ok(selection) = line.trim().parse::<usize>() else {
            println!("Selection must be a number.");
            continue;
        };
        if !(1..=devices.len()).contains(&selection) {
            println!("Selection must be between 1 and {}.", devices.len());
            continue;
        }

        return Ok(devices[selection - 1].selector());
    }
}
