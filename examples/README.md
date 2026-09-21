# Framework examples

This directory contains the self-contained public examples for the Phoxal Framework.
The examples use local path dependencies so a framework change can be tested before any package is released.
They do not depend on another source repository.

## Example map

| Area | Example | What it proves |
| --- | --- | --- |
| Component | `components/passive-caster/` | A targetless authored component with assets and no implicit driver process |
| Component | `components/bench-motor/` | A model-only actuator and encoder component used twice in one robot |
| Component | `components/bench-imu/` | A model-only sensor component with an explicitly mapped native signal |
| Service | `services/counter/` | A service-owned Protobuf contract, contract-only library, private Runtime binary, typed config, and direct Runtime tests |
| Runtime | `robots/minimal/` | The smallest complete robot brain and document |
| Robot | `robots/composed/` | A standalone robot workspace with local components, a reusable service, model mounts, and its own lockfile |
| Robot and scenario | `robots/moving-rover/` | A complete service graph and the finite `ForwardTurnStop` native movement scenario |

## Workspace shapes

`examples/Cargo.toml` is the buildable example workspace.
It contains the minimal brain and the counter service plus its contract package.
Run its tests with:

```sh
cargo test --manifest-path examples/Cargo.toml --workspace --all-targets --locked
```

`robots/composed/` and `robots/moving-rover/` are independent Cargo workspaces with their own lockfiles.
They exercise the same root-package and lock ownership expected from a standalone robot source tree.
They remain inside this repository so every referenced source, model, contract, and asset is reviewable together.

The model-only component packages intentionally have no Rust target.
They are prepared in a temporary build closure by `cargo-phoxal` and are not added to the ordinary example Cargo workspace.

## Minimal Runtime

`robots/minimal/` contains a unit-configured brain with unit State, no inputs, no outputs, and no behavioral service stack.
It is the smallest complete Runtime process and robot document.

From that directory, run the framework tool directly from this checkout:

```sh
cd examples/robots/minimal
cargo run --manifest-path ../../../phoxal/cargo/Cargo.toml -- check --locked
```

## Reusable service

`services/counter/contract/` owns the counter Protobuf schema and generated port descriptor.
`services/counter/` exposes only the generated contract from its library and keeps configuration, inputs, outputs, and Runtime implementation private to the executable.
Its unit tests exercise typed initialization and invocation without starting a supervisor or transport process.

Run only this package with:

```sh
cargo test --manifest-path examples/Cargo.toml --package example-counter-service --locked
```

## Authored components

`components/passive-caster/` contains only its Cargo metadata, component document, and asset.
It proves that passive authored data does not need an invented Rust library or driver executable.

Validate its publication closure without modifying the authored directory:

```sh
cargo run --manifest-path phoxal/cargo/Cargo.toml -- \
  publish component example-passive-caster \
  --path examples/components/passive-caster \
  --dry-run
```

`components/bench-motor/` and `components/bench-imu/` are model-only composition examples.
Their capabilities map semantic component ports to explicit native actuator, joint, and site targets.

## Composed robot

`robots/composed/` selects the counter service and mounts two motor instances plus one IMU instance.
It demonstrates repeated component instances, distinct mount sites, direct contract reuse, a root-owned lockfile, and an authored simulation scene.

Run its project validation with:

```sh
cd examples/robots/composed
cargo run --manifest-path ../../../phoxal/cargo/Cargo.toml -- check --locked
```

Framework unit tests parse these maintained documents and assert their model, capability, target, signal, mount, and scene shapes.

## Moving rover

`robots/moving-rover/` contains a four-wheel model, component drivers, kinematics, World, Safety, Motion, and the `ForwardTurnStop` scenario.
It is the end-to-end example for a finite controlled run with arm, forward, turn, stop, disarm, withdrawal, typed command replies, service-state captures, native displacement, yaw, and stopped-state evidence.

Prepare and inspect it with:

```sh
cd examples/robots/moving-rover
cargo run --manifest-path ../../../phoxal/cargo/Cargo.toml -- check --locked
cargo run --manifest-path ../../../phoxal/cargo/Cargo.toml -- \
  simulation scenario list --locked
```

After installing the supported simulator application, run the native scenario with:

```sh
cargo phoxal simulation install
cargo run --manifest-path ../../../phoxal/cargo/Cargo.toml -- \
  simulation scenario run ForwardTurnStop --locked --release
```

The native scenario is an explicit environment-dependent acceptance run.
Ordinary framework CI compiles and tests the source-only examples without claiming native movement or graphics evidence.

## Fresh setup

The framework declares Rust `1.88` as its minimum supported toolchain.
Install that toolchain with rustup when an older default is active:

```sh
rustup toolchain install 1.88.0
rustup override set 1.88.0
```

The example workspace includes the Phoxal sparse registry configuration required by direct Cargo package resolution.
The examples themselves select local framework packages, so no framework release is required to compile them from this checkout.

The released developer tool can be installed independently when testing the published workflow:

```sh
cargo install cargo-phoxal \
  --index sparse+https://phoxal.github.io/registry/ \
  --version 0.0.0-dev.6 \
  --locked
```
