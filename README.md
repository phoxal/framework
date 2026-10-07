# Phoxal Framework

The reusable Phoxal SDK: shared robotics contracts, runtime and session primitives, typed authoring macros, and local binding generation.
Linux and macOS are supported.
Windows and other operating systems are unsupported and unqualified.

The workspace publishes `phoxal`, `phoxal-build`, and `phoxal-macros`.
The developer command, supervisor application, official services, official components, and native simulator have their own repositories.
Full four-wheel native qualification belongs to [robot-rover](https://github.com/phoxal/robot-rover).

## Robot authoring

A robot selects its supervisor and participants through `robot.yaml`.
The supervisor uses the same local path or pinned Git source selection as participants and is acquired independently of the robot's Rust library dependencies.
Prepare the selections with `cargo phoxal prepare`, then use the ordinary Cargo build and test workflow.
The build helper reads the exact resolved prepared composition, local component capabilities, and prepared contracts; it never acquires packages or invokes Cargo.
Framework and participant wire messages share Rust authoring macros, standard Protobuf encoding, and compiler-resolved schema records.
Production builds do not invoke protoc; independent protoc conformance belongs to the test fixtures.

```rust
phoxal::api!();
mod conversions;
mod runtime;
fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<runtime::Brain>()
}
```

The runtime type uses one `#[phoxal::endpoints]` declaration and one inherent `#[phoxal::runtime]` implementation.
A conversions module contains ordinary Rust `From`/`TryFrom` implementations only when distinct endpoint payloads require a conversion.
The generated API attaches these conversions to the brain's normal invocation, with its existing commit/discard and reset lifecycle, without another process or launch API.
Input capture stamps and lease bounds remain source-owned.

SDK consumer profiles are described in [phoxal/README.md](phoxal/README.md).
Local binding generation is described in [phoxal-build/README.md](phoxal-build/README.md).
The [developer tool](https://github.com/phoxal/cargo), [supervisor](https://github.com/phoxal/supervisor), [services](https://github.com/phoxal/services), [components](https://github.com/phoxal/components), and [simulator](https://github.com/phoxal/simulator) document their own workflows.

## Maintainer qualification

Ordinary development can use `cargo test -p phoxal --lib`.
The strict CI commands below add `--locked` to require an up-to-date committed lockfile.
The SDK unit lane is prepare-free:

```sh
cargo test --locked -p phoxal -p phoxal-build -p phoxal-macros --lib
cargo test --locked -p phoxal --test unit --test runtime_behavior_observers
cargo test --locked -p phoxal --test compile --features scenario
cargo fmt --all -- --check
cargo clippy --locked -p phoxal -p phoxal-build -p phoxal-macros --all-targets --features phoxal/scenario -- -D warnings
RUSTDOCFLAGS="-D warnings" cargo doc --locked -p phoxal -p phoxal-build -p phoxal-macros --no-deps --features phoxal/scenario
```

Small compiled fixtures under `tests/` retain the SDK's authoring, standard-contract, and generated-client acceptance.
Fixtures importing another participant need the public tool's preparation first; they are not prerequisites of the prepare-free unit lane.
For the generated-client fixture, run `cargo phoxal prepare` in `tests/contracts/client` before compiling or running that client.
The provider in that fixture is a minimal SDK-only executable, not an official service implementation.
The `standard-plus-custom` warm-edit test builds its own cold scratch project and compares regenerated contracts through a real Cargo build.
Supervisor process tests and tool acquisition/compiler tests live with those applications.

Versions remain independent package release selections.
Compatibility follows the interfaces each application consumes, rather than an equal-version release train.
Package archives are published to crates.io through standard release-plz workflows.

## Publication

Review and merge release-plz package version and generated changelog PRs before publication.
The release job runs after successful main-branch CI and publishes to crates.io using the organization publication credential.
Publish `phoxal-macros` and `phoxal-build` before the `phoxal` package that depends on them.
Verify the public archives and clean consumer builds before releasing applications that consume a new SDK.
Packages retain independent versions, and compatibility follows the interfaces consumed by each operation.
