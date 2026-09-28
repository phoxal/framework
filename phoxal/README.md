# phoxal

The Phoxal framework runtime, generated service contracts, and public session client live in this crate.

Source preparation, Cargo orchestration, and immutable bundle assembly belong to the framework-owned `cargo-phoxal` package.
Native model composition belongs to the independent simulator application.
The runtime consumes the completed bundle manifest and never parses `robot.yaml`, `component.yaml`, or native model source files.

## Consumer profiles

The crate has one compatibility identity and one Cargo feature per supported executable role.
Features select compile-time surfaces and dependencies, while process ownership and the constructible API enforce authority at runtime.
The base profile is inert data: contracts, schemas and codecs, geometry helpers, protocol identities, and artifact records, without a runner or transport.

A robot Runtime package uses the default feature set:

```toml
[dependencies]
phoxal = "0.68"
```

An application attaching to a running execution:

```toml
[dependencies]
phoxal = { version = "0.68", default-features = false, features = ["session"] }
```

Scenario authoring belongs to the test tier, so a robot selects it as a dev dependency:

```toml
[dev-dependencies]
phoxal = { version = "0.68", features = ["scenario"] }
```

A public contract consumer with no transport or runner takes the base profile:

```toml
[dependencies]
phoxal = { version = "0.68", default-features = false }
```

Prepared-product generation is selected in build dependencies only:

```toml
[build-dependencies]
phoxal = { version = "0.68", default-features = false, features = ["build"] }
```

The `runtime` profile is the default and provides the synchronous Runtime macros, typed inputs and outputs, runner, and required transport.
The `session` profile provides only the public logical-session client and its Protobuf transport.
The `scenario` profile provides scenario authoring and the fixture client without the participant runner or transport.
The `build` profile provides local prepared-product generation for build scripts.
The supervisor executable owns its host implementation privately and consumes the reusable `runtime`, `scenario`, and `session` APIs.

`#[phoxal::message]` and `#[phoxal::endpoints]` define runnable package contracts in Rust.
`phoxal::contracts` contains stable shared robotics, geometry, and component vocabulary; service-specific records remain with their owner.
An input payload that crosses a process boundary uses `#[phoxal::message]`, even when it is private to the consumer and needs no authored package or version.
An algorithm-only struct that never crosses that boundary remains ordinary Rust.
Exported payloads and the endpoint struct share one package declaration in `#[phoxal::messages(package = "owner.v1")] mod v1 { ... }`; a provided operation's identity defaults to that package plus its UpperCamelCase field name, while a call always names the provider's operation explicitly.
The battery and lidar vocabularies are schemas only: no capability derives their endpoints yet and the native simulator implements no provider for them.
Enum fields use typed Rust enums with explicit stable discriminants; unknown numeric values and missing payload-enum variants fail decoding.
`phoxal::build::api(BuildApiConfig::default())` reads exact prepared participant products in an ordinary robot build script.
Place `phoxal::api!();` once at a binary or library crate root to attach its generated `api` module.
The helper reads local prepared products only, so prepare selected registry or Git participants with `cargo phoxal prepare` before the first bare Cargo build.
The project compiler owns source preparation and project validation inside the `cargo-phoxal` package.
When a selected output type differs from an input expectation, place an ordinary `From` or `TryFrom` implementation in the robot's `src/conversions.rs`.
Preparation generates one robot-owned adapter runtime for those connections; direct identity connections keep their original route.
Conversion errors fault that adapter before it publishes a replacement value, preserving the source timestamp of successful conversions.

Use <https://docs.rs/phoxal> as the authority for the published Rust API.
Visit <https://phoxal.com> for the project vision and public introduction.
This repository's source and documentation are the authority for implementation, architecture, and compile-time contract details.

## License

AGPL-3.0-only.
A commercial license is available; see the repository root.
