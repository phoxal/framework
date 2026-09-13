# Phoxal MuJoCo simulator

This directory is an independently built simulator application.
It owns the native MuJoCo dependency and has its own `Cargo.lock`.
It is not a dependency of a robot project or of the generic Phoxal runtime.

The package exposes one presentation-neutral native coordinator and one public-session authority coordinator.
The headless and desktop presentations share the same fixed native advancement path.
The public-session coordinator uses only `phoxal::session::Simulation`, the public simulation Protobuf messages, the owned robotics/kinematics/motion contracts, and the native `phoxal-mujoco` owner.
It carries the authenticated session, authority grant, execution, timeline, generation, boundary, lease, provider set, actuation bindings, and exact run provenance through every operation.
It never admits a missing provider or a missing native actuator by supplying a default.
The desktop feature uses egui and exposes Play/Pause, Step, Step N, Reset, Stop, camera selection, scene points, identities, progress, and typed failure diagnostics.
The current executable exposes only the explicit `--native-core` smoke path; the public-session coordinator is a library API and is not yet wired to an executable command.

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

The `native` feature is enabled by default for the executable.
`cargo check --manifest-path simulators/mujoco/Cargo.toml --no-default-features --lib` and the authority tests compile without a MuJoCo installation, which keeps public-session protocol work independently testable.

## Run

The model path is also the explicit resource-root boundary.
Every regular file below its parent directory is admitted into the closed VFS, while symlinks are refused.

```sh
cargo run --manifest-path simulators/mujoco/Cargo.toml -- simulators/mujoco/fixtures/hinge.xml --native-core --steps 10
cargo run --manifest-path simulators/mujoco/Cargo.toml -- simulators/mujoco/fixtures/hinge.xml --native-core --duration 0.1
cargo run --manifest-path simulators/mujoco/Cargo.toml --features desktop -- simulators/mujoco/fixtures/hinge.xml --native-core --desktop --steps 100
```

`--duration` must resolve to a positive integral number of source-authored native quanta.
The explicit `--native-core` mode is a deterministic native smoke run and uses no robot bundle or supervisor.
Without that flag, the executable refuses a model-only invocation so a local hold provider cannot be mistaken for a robot-backed simulation.
Bundle-backed runs are owned by `cargo phoxal simulation run`, which provisions or reuses this independent application and must supply the immutable simulation definition and native scene bindings before constructing `RemoteSceneRun`.
