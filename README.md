# Phoxal Framework

Rust workspace for the Phoxal Framework libraries, execution supervisor, and official Runtime services and component drivers.

Phoxal is pre-1.0 and evolving.
Visit <https://phoxal.com> for the project vision and public introduction.
The published Rust API is documented at <https://docs.rs/phoxal>.
This repository and its source are the authority for current framework implementation and architecture details.

## Repository

- `phoxal/` - the framework facade and runtime library
- `supervisor/` - the framework execution supervisor
- `services/`, `components/` - official services and drivers, with their owned contracts beside private binary implementations
- `phoxal/cargo/` - the registry-aware project and publication command
- `tests/` - internal compile-contract fixtures and one four-wheel native qualification robot

### Fixture purposes under `tests/`

Participant fixtures are binary-only: `tests/supervisor-runtime`, `tests/contracts/robot`, `tests/hardware/driver`, and `tests/robot` each own their contract and runtime modules behind the executable, because a participant's public surface is its compiled binary, not a distributable library.
Integration tests that need the same authored contract include it with a local `#[path]` attribute instead of adding a library target.
The one exception is `tests/contracts/schema-proof`: its library is an unpublished codec/descriptor oracle shared by its proof executable and tests, not a service API; do not copy that shape for runtime participants.
`tests/adapted-robot` is a deliberately standalone Cargo workspace (its own lockfile) proving robot-owned conversion adapters end to end.
At least one client fixture (`tests/contracts/client`) always consumes bindings generated from a selected compiled artifact, so local source sharing cannot conceal a generation defect.

Runnable packages author endpoint contracts and payloads in Rust with `#[phoxal::endpoints]` and `#[phoxal::message]`.
`phoxal::contracts` supplies shared robotics and component contracts, while each service owns its specialized exported records and private input expectations.
A driver's `component.yaml` declares its native model and capabilities; those capabilities derive standard endpoints.
The robot's `robot.yaml` selects exact participants and connects endpoint names without duplicating their types.
`cargo phoxal prepare` obtains selected participant binaries, extracts their compiled contracts, and generates local robot bindings before the robot builds.
The robot's `build.rs` calls `phoxal::build::api`, and `phoxal::api!();` attaches the generated instance API to its crate root.
For differently typed latest observations, ordinary `From` or `TryFrom` code in the robot's `src/conversions.rs` is compiled into a generated robot-owned adapter runtime.
Identical contract connections remain direct.
No selected service or component library is added to the robot's Cargo dependencies for communication.

Hardware-only projects and `cargo-phoxal` build and run without MuJoCo or a simulator installation.

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and contribution requirements.

## Install the developer tool

Install or update the framework-owned `cargo-phoxal` package from the Phoxal registry:

```sh
cargo install cargo-phoxal \
  --index sparse+https://phoxal.github.io/registry/ \
  --version 0.0.0-dev.6 \
  --locked
```

Pre-release tools must be selected explicitly because Cargo does not choose them for a plain `cargo install`.
The current `cargo-phoxal` release is `0.0.0-dev.6`.
It consumes the `0.0.0-dev.2` framework package set.
Later incompatible development trains increment only the final pre-release counter.
The release order begins with `phoxal-build`, `phoxal-macros`, and `phoxal`, then continues through services, components, the supervisor, and `cargo-phoxal`.
The build helper is required first because `phoxal` exposes the same Protobuf authoring implementation to consumer build scripts while also using it for its own generated contracts.

The installed executable is invoked as a Cargo subcommand:

```sh
cargo phoxal --help
cargo phoxal check --locked
```

`cargo-phoxal` is versioned and published independently, but it remains in this repository because it directly owns the project compiler, bundle format integration, scenario protocol, and supervisor launch contract.
Keeping those coupled changes in one owner repository prevents version-skewed adapters and duplicate project models.

Hardware-only development does not require MuJoCo.
Simulation uses the independently managed Phoxal Simulator application, while `cargo-phoxal` owns installing MuJoCo, building the matching registry package, and maintaining the user installation.
The first command that needs native execution provisions the verified application automatically.
Compilation-only and test-listing commands do not provision it.

```sh
cargo phoxal simulation status
cargo phoxal simulation install
```

Automatic provisioning and the explicit installer download and verify the supported MuJoCo distribution, install the exact simulator release from the Phoxal registry, retain its licenses and provenance, and keep native files outside robot projects.
Use `cargo phoxal simulation upgrade` to replace the managed installation and `cargo phoxal simulation uninstall` to remove it.
An existing official MuJoCo distribution can be selected explicitly with `--mujoco-distribution <path>`.

The repository intentionally has no maintained user examples while its pre-1.0 authoring and runtime contracts are still changing.
Before pushing a change that can affect project preparation, runtime transport, components, or simulation, use the internal four-wheel robot in the root Cargo workspace.
It contains four actuator/encoder components, a disarmed brain, a robot-local controller, World and Navigation services, one robot-owned revision conversion, and the finite `ForwardTurnStop` scenario.

Build the current framework tool, then run the ordinary Cargo test workflow:

```sh
cd tests/robot
../../target/debug/cargo-phoxal test --locked --offline --no-run
../../target/debug/cargo-phoxal test --locked --offline -- --list
../../target/debug/cargo-phoxal test forward_turn_stop -- --nocapture
```

Pass `--simulator <path>` to qualify an explicitly built source simulator instead of the managed application.
This native qualification is a local pre-push gate for now.
It is not a public example, a supported robot template, or a substitute for focused deterministic tests.

## Releases

Every published library, contract, executable, and simulator package owns a semantic version in its Cargo manifest.
During the current experimental reset, coherent framework trains use the monotonic `0.0.0-dev.N` sequence and exact internal requirements.
The source commit is not embedded in the package version.
Every merge to main updates a reviewable release-plz version pull request, and its diff is the current list of packages the next release train would publish; unrelated packages remain unchanged after this reset train.
Starting the train remains a deliberate maintainer step because the static reviewed registry has no publication API or release state that release-plz can observe; the train merges the exact reviewed version pull request head and publishes every version it contains.
The framework does not publish rewritten packages to crates.io.
The current build-script API proof uses a temporary standard sparse registry and exact package archives.
Public publication and release qualification are separate work after the package model settles.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

## License

AGPL-3.0-only. See [LICENSE](LICENSE) and [COMMERCIAL.md](COMMERCIAL.md).
