// The package that cannot support a target owns that truth (organization#951,
// Decision 7). This controller links Webots' `libController` at build time, and
// Cyberbotics ships it only for the desktop platforms Webots itself runs on -
// so the unsupported targets fail here, legibly, instead of as a linker error
// or as release metadata the build never consults.
#[cfg(target_env = "musl")]
compile_error!(
    "phoxal-simulator-webots-controller does not support musl targets: it links Webots' \
     libController, which Cyberbotics ships only against glibc. Simulation runs on a \
     desktop host, not on a musl robot image."
);

#[cfg(all(target_arch = "aarch64", target_os = "linux", target_env = "gnu"))]
compile_error!(
    "phoxal-simulator-webots-controller does not support aarch64-unknown-linux-gnu: \
     Webots ships no aarch64 Linux distribution, so there is no libController to link \
     against. Run simulation on an x86_64 Linux or an Apple Silicon macOS host."
);

mod capabilities;
mod webots_controller;

fn main() -> phoxal::Result<()> {
    webots_controller::run()
}
