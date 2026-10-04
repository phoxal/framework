//! A private input whose identity must be qualified by the owning
//! package even when the binary target name matches another package's.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

mod input;

fn main() {
    let _ = input::Reading::default();
}
