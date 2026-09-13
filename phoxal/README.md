# phoxal

The Phoxal framework runtime, protocol contracts, typed ports, and public session client live in this crate.

Source preparation, Cargo orchestration, native model composition, and immutable bundle assembly belong to the independent `phoxal-project` and simulator packages.
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
phoxal = { version = "0.68", default-features = false, features = ["port"] }
```

The `runtime` profile is the default and provides the synchronous Runtime macros, typed inputs and outputs, runner, and required transport.
The `session` profile provides only the public logical-session client and its Protobuf transport.
The supervisor executable owns its host implementation privately and consumes the reusable `runtime` and `session` APIs.
The `port` and `protocol` profiles provide independent descriptor and protocol contracts without a runner or host implementation.

The project compiler is the only owner of authored source parsing and project validation.
It is exposed through `cargo phoxal` and is implemented in `phoxal-project` plus the `cargo-phoxal` executable.

Use <https://docs.rs/phoxal> as the authority for the published Rust API.
Visit <https://phoxal.com> for the project vision and public introduction.
This repository's source and documentation are the authority for implementation, architecture, and compile-time contract details.

## License

AGPL-3.0-only.
A commercial license is available; see the repository root.
