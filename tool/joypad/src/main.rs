// The package that cannot support a target owns that truth (organization#951,
// Decision 7). `gilrs` reaches the gamepad through libudev, which musl targets
// do not provide, so the failure belongs here as a readable compiler error
// rather than in release metadata the build never consults.
#[cfg(target_env = "musl")]
compile_error!(
    "phoxal-tool-joypad does not support musl targets: its gamepad backend (gilrs) \
     needs libudev, which is glibc-only. Build the joypad tool for a gnu target."
);

mod joypad;

fn main() -> phoxal::Result<()> {
    phoxal::run::<joypad::ToolJoypad>()
}
