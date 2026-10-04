//! A second binary of the same package: its private input must derive a
//! distinct identity from the driver binary's identically named module.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

mod input;

fn main() {
    let _ = input::Reading::default();
}
