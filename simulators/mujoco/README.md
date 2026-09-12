# Phoxal MuJoCo simulator

This directory is an independently built simulator application.
It owns the native MuJoCo dependency and has its own `Cargo.lock`.
It is not a dependency of a robot project or of the generic Phoxal runtime.

The application currently provides a headless fixed-step skeleton.
It loads a closed MJCF directory, compiles it through `phoxal-mujoco`, advances the same scene owner used by future desktop controls, and prints a machine-readable terminal summary.

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
```

`--duration` must resolve to a positive integral number of source-authored native quanta.
The current skeleton does not connect a supervisor or render a viewport.
Those public session and provider boundaries belong to the controlled integration workstream.
