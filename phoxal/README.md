# phoxal

The Phoxal framework runtime, generated service contracts, and public session client live in this crate.

Source preparation, Cargo orchestration, and immutable bundle assembly belong to the framework-owned `cargo-phoxal` package.
Native model composition belongs to the independent simulator application.
The runtime consumes the completed bundle manifest and never parses `robot.yaml`, `component.yaml`, or native model source files.

## Consumer profiles

The crate has one compatibility identity and one Cargo feature per supported process role.
Features select compile-time surfaces and dependencies, while process ownership and the constructible API enforce authority at runtime.

```toml
# A robot Runtime package.
phoxal = "0.68"

# An application attaching to a running execution.
phoxal = { version = "0.68", default-features = false, features = ["session"] }

# A public contract consumer with no transport or runner.
phoxal = { version = "0.68", default-features = false, features = ["contract"] }
```

The `runtime` profile is the default and provides the synchronous Runtime macros, typed inputs and outputs, runner, and required transport.
The `session` profile provides only the public logical-session client and its Protobuf transport.
The supervisor executable owns its host implementation privately and consumes the reusable `runtime` and `session` APIs.
The `contract` profile provides generated method descriptors and typed call and observation handles without a runner or host implementation.
The `protocol` profile provides the public Protobuf transport contracts.

`phoxal::build::api(BuildApiConfig::default())` reads package-local `api/` files and exact prepared robot selections in an ordinary Cargo build script.
Place `phoxal::api!();` once at a binary or library crate root to attach its generated `api` module.
The helper reads local sources only, so prepare selected registry or Git participants with `cargo phoxal prepare` before the first bare Cargo build.
The project compiler owns source preparation and project validation inside the `cargo-phoxal` package.

Use <https://docs.rs/phoxal> as the authority for the published Rust API.
Visit <https://phoxal.com> for the project vision and public introduction.
This repository's source and documentation are the authority for implementation, architecture, and compile-time contract details.

## License

AGPL-3.0-only.
A commercial license is available; see the repository root.
