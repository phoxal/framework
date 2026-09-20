# Runtime rewrite examples

This directory is a maintained, standalone Cargo workspace for the Runtime rewrite.

It is intentionally source-only and uses the framework crates by path so CI can compile the examples before any package is published.

The same files can switch to published `phoxal` registry coordinates by replacing those path dependencies in the example workspace.

## Fresh-machine setup

The recipe assumes a supported macOS or Linux host and a clean checkout of the framework and robot project repositories.

### 1. Install the toolchain

Framework declares Rust `1.88` as its minimum supported toolchain in the workspace manifest.

Install and select that toolchain with:

```sh
rustup toolchain install 1.88.0
rustup override set 1.88.0
```

Rustup and Cargo own the compiler and package build tools, while the framework owns the minimum version required by its public crates.

### 2. Configure the Phoxal registry

The retained static sparse registry is served at `https://phoxal.github.io/registry/`.

For `cargo install` and direct Cargo commands, merge this table into the user's `$CARGO_HOME/config.toml`, which defaults to `~/.cargo/config.toml`.

```toml
[registries.phoxal]
index = "sparse+https://phoxal.github.io/registry/"
```

Do not replace unrelated user Cargo configuration when adding this table.

The registry owns immutable package archives, checksums, and reviewed publication, while framework owns package preparation and the `cargo-phoxal` executable.

This example workspace keeps the same table in `.cargo/config.toml` so direct Cargo commands have an explicit project registry configuration.

### 3. Install cargo-phoxal for the user

Install the released framework tool into Cargo's user binary directory with:

```sh
cargo install cargo-phoxal --registry phoxal --version 0.0.0-dev.3 --locked
```

Cargo requires the explicit version because the development train is a pre-release.

Ensure Cargo's install directory is on `PATH` as documented by `cargo install`.

`cargo-phoxal` is the source-development entry point and does not install a second `phoxal` executable or the archived `phoxal-cli` repository.

The tool supplies the Phoxal registry index to the Cargo commands it owns, so a robot workspace may keep registry configuration in its own `.cargo/config.toml` only when it also invokes direct Cargo commands.

### 4. Install contract and native tools only when their owner requires them

Every contract owner uses the lightweight `phoxal::build` facade over `phoxal-build`, which supplies a pinned `protoc-bin-vendored` compiler during Cargo builds. Owner build manifests declare `phoxal` with the `build` feature in `[build-dependencies]` and do not name `phoxal-build` directly.

Normal Runtime, hardware, and `cargo-phoxal` workflows therefore do not require a system `protoc` installation.

Buf is the contract owner's CI and schema-linting tool, not a Runtime installation dependency.

Install Buf separately only when editing or reviewing Protobuf contracts, and use the version required by the repository's CI configuration.

Hardware-only projects do not install MuJoCo or a renderer.

Simulation users provision the supported native distribution and released application through the developer tool:

```sh
cargo phoxal simulation install
cargo phoxal simulation status
```

The independent simulator owns physics, rendering, composition, and native provider behavior.
`cargo-phoxal` owns downloading and verifying MuJoCo, building the exact registry release, preserving licenses and provenance, and maintaining the user installation.

The framework owns the Runtime and supervisor APIs, contract build helper, project compiler, and developer tool, while Rustup, Buf, Protobuf, and MuJoCo remain the owners of their respective host tools.

## Example matrix

The examples contain one canonical contract, one reusable service, four small robot packages, and one standalone local passive component.

The passive component is deliberately not a Cargo workspace member because its authored package has no Rust target; its maintained qualification boundary is the cargo-phoxal publication dry run.

| Example | Authored files | Qualification command |
| --- | --- | --- |
| Minimal brain | `robots/robot-alpha/` | `cargo phoxal check --locked` from that directory |
| Reusable service | `services/counter/` | `cargo test --workspace --all-targets` |
| Local passive component | `components/passive-caster/` | `cargo phoxal publish component example-passive-caster --path ... --dry-run` |
| Robot package inside a workspace | `robots/workspace-robot/` with the repeated-component model fixture | `cargo phoxal check --locked` from that directory |
| Two robot packages in one repository | `robots/robot-alpha/` and `robots/robot-beta/` | Run the same check from each directory |
| Native movement scenario | `robots/simulation-rover/` | `cargo phoxal simulation scenario run ForwardTurnStop --locked --release` |

No creation or scaffolding command is required.

### Minimal brain

`robots/robot-alpha/` contains the smallest complete robot application.

Its `src/main.rs` implements the real `phoxal::runtime::Runtime` API, returns unchanged unit State, and has no mission policy or behavioral service.

Its `robot.yaml` keeps `services: {}` explicit and places the mandatory brain beside the document through the ordinary root Cargo package.

Run it from the package directory with:

```sh
cd robots/robot-alpha
cargo phoxal check --locked
```

The check discovers this `robot.yaml`, resolves the root package, validates its Runtime artifact contract, and verifies the selected supervisor through the root Cargo lock.

### Reusable service

`services/counter/contract/` owns `proto/example/counter/v1/counter.proto` and uses `phoxal::build::compile_protos` from its build script.

The service re-exports the generated `counter::STATE` descriptor and `CounterState` message from that canonical contract package.

The service library exports only its generated messages and ports.
Its executable owns private configuration, input, output and runtime modules.

Its direct unit test calls `initialize` and `invoke`, so it checks the same service implementation without starting a supervisor or transport.

The workspace robot imports the canonical contract directly for its brain and selects the service package with the Cargo dependency key `counter` through `services: { counter: {} }`.

Run all example tests with:

```sh
cargo test --workspace --all-targets
```

### Local passive component

`components/passive-caster/` contains only an authored `Cargo.toml`, `component.yaml`, and declared profile asset.

It intentionally has no authored Rust source and no executable driver.

Use the framework publication preparer to validate and package it without changing the source tree:

```sh
cargo phoxal publish component example-passive-caster \
  --path components/passive-caster \
  --dry-run
```

The preparer adds the inert `_cargo/lib.rs` carrier and technical Cargo fields only in temporary staging.

The component remains a data package and never becomes an implicit Runtime process.

### A robot package inside a workspace

`robots/workspace-robot/` is a standalone nested Cargo workspace beside the reusable service and contract packages.

Its root Cargo package owns the brain, the explicit service and component dependencies, the root Cargo lock, and the mandatory supervisor dependency.

Its `robot.yaml` is the maintained repeated-component authoring fixture from the MuJoCo plan.

The fixture selects the `bench_motor` dependency twice with distinct `mount_site` values and selects `bench_imu` once.

Each component definition uses `model: { file: model.xml, root_body: mount }` and explicit semantic `target` fields, with the accelerometer also naming its `signals`.

The robot owns `model.xml` and the application scene lives at `simulation/scene.xml`.

The brain imports the canonical generated contract package directly, so the project check exercises the real root Cargo graph and project preparation rather than generating robot-specific bindings.

The complete project check from its directory is:

```sh
cd robots/workspace-robot
cargo phoxal check --locked
```

The project compiler captures a temporary shadow workspace and stages inert Cargo library carriers for the two targetless local component packages before Cargo metadata.

The authored component directories and robot manifest remain unchanged, and the nested workspace lock is the one logical lock retained by this package.

The command is therefore a verified source-development gate for this fixture.

The automated fixture test checks the canonical artifact-format document types and exact model, target, signal, mount-site, and scene shape without pretending that native composition has passed.

The current-runtime alpha and beta packages continue to pass `cargo phoxal check --locked` in CI.

The component packages are driver-free authoring packages and do not create Runtime processes.

Their model composition and native provider behavior are separate MuJoCo gates and are not implied by a successful Cargo check.

### Native movement scenario

`robots/simulation-rover/` is a standalone, path-based robot project that exercises the framework checkout before registry publication.
It contains a small four-wheel robot model, one scene, and the `ForwardTurnStop` scenario.
Its dependencies point at local framework packages deliberately, so source changes are tested without first releasing a package or updating the public rover repository.

Prepare the managed simulator once, then run the scenario from that directory:

```sh
cargo phoxal simulation install
cd robots/simulation-rover
cargo phoxal check --locked
cargo phoxal simulation scenario run ForwardTurnStop --locked --release
```

For simulator source development, bypass the managed installation explicitly:

```sh
cargo phoxal simulation scenario run ForwardTurnStop --locked --release \
  --simulator /absolute/path/to/phoxal-simulator
```

The scenario validates the real source-to-bundle path, supervisor and simulator intercommunication, native displacement and yaw, and a stopped terminal state.
It is a maintained end-to-end development gate, while the simpler examples remain focused on isolated authoring and package behavior.

### Two robot packages in one repository

`robots/robot-alpha/` and `robots/robot-beta/` are separate Cargo workspace members with separate `robot.yaml` documents and distinct robot IDs.

They share the example workspace's `Cargo.lock`, but each selects its own root brain and execution graph.

Run both checks independently:

```sh
cd robots/robot-alpha
cargo phoxal check --locked
cd ../robot-beta
cargo phoxal check --locked
```

The two packages do not create member-local locks, and neither robot's identity is inferred from the other package.

Use separate Cargo workspaces in one repository when independent lockfiles are required.

## Validation

Framework CI runs the example workspace with `cargo test --workspace --all-targets`, runs `cargo phoxal check --locked` for each robot package, and performs the passive-component dry-run publication.

The local equivalent from the framework checkout is:

```sh
cargo test --manifest-path examples/runtime-rewrite/Cargo.toml --workspace --all-targets
for robot in \
  examples/runtime-rewrite/robots/robot-alpha \
  examples/runtime-rewrite/robots/robot-beta \
  examples/runtime-rewrite/robots/workspace-robot \
  examples/runtime-rewrite/robots/simulation-rover; do
  (cd "$robot" && cargo run --manifest-path ../../../../phoxal/cargo/Cargo.toml -- check --locked)
done
cargo test --manifest-path examples/runtime-rewrite/Cargo.toml \
  --package example-mujoco-fixture-check
cargo run --manifest-path phoxal/cargo/Cargo.toml -- \
  publish component example-passive-caster \
  --path examples/runtime-rewrite/components/passive-caster \
  --dry-run
```

The outer example workspace and standalone robot `Cargo.lock` files are committed so `--locked` remains meaningful for fresh checkouts.
