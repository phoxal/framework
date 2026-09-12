# Phoxal MuJoCo simulator

This directory is an independently built simulator application.
It owns the native MuJoCo dependency and has its own `Cargo.lock`.
It is not a dependency of a robot project or of the generic Phoxal runtime.

The package exposes one presentation-neutral `SimulationCore` library and provides headless finite runs plus an optional desktop presentation.
Both entry points use the same library coordinator, controlled provider exchange, and fixed native advancement path.
The desktop feature uses egui and exposes Play/Pause, Step, Step N, Reset, Stop, camera selection, scene points, identities, progress, and typed failure diagnostics.

## Native setup

The application uses `mujoco-rs` 6.0.1, which is the binding release for MuJoCo 3.12.0.
The native MuJoCo distribution is not vendored in this repository.
Set `MUJOCO_DYNAMIC_LINK_DIR` to the absolute path of the native distribution's `lib` directory before building.
Set the platform dynamic-loader path as well, such as `DYLD_LIBRARY_PATH` on macOS or `LD_LIBRARY_PATH` on Linux.

For example, a Linux installation can use:

```sh
export MUJOCO_DYNAMIC_LINK_DIR=/opt/mujoco-3.12.0/lib
export LD_LIBRARY_PATH="$MUJOCO_DYNAMIC_LINK_DIR"
cargo check --manifest-path simulators/mujoco/Cargo.toml
```

The framework workspace intentionally does not enable the library's `native` feature by default.
This allows hardware-only packages and generic runtime tests to build without MuJoCo.

## Run

The model path is also the explicit resource-root boundary.
Every regular file below its parent directory is admitted into the closed VFS, while symlinks are refused.

```sh
cargo run --manifest-path simulators/mujoco/Cargo.toml -- simulators/mujoco/fixtures/hinge.xml --steps 10
cargo run --manifest-path simulators/mujoco/Cargo.toml -- simulators/mujoco/fixtures/hinge.xml --duration 0.1
cargo run --manifest-path simulators/mujoco/Cargo.toml --features desktop -- simulators/mujoco/fixtures/hinge.xml --desktop --steps 100
```

`--duration` must resolve to a positive integral number of source-authored native quanta.
Headless output is one JSON terminal summary and exits nonzero when required provider admission or native progress fails.
The application still does not connect a supervisor public session or implement offscreen camera sensors.
Those public session and sensor-provider boundaries remain integration gaps outside this local native-core slice.
