# phoxal-build

Build-time support for package-owned Protobuf contracts.

The package-local entry point is `phoxal::build::api(BuildApiConfig::default())` in an ordinary `build.rs`.
It discovers `api/**/*.proto` and the package's authored service declaration: `service.yaml` for a service, the embedded endpoint sections of `component.yaml` for a component, or the `brain` section of `robot.yaml` for a robot project.
For a robot package it also resolves the exact sources selected by `robot.yaml` under its prepared `.phoxal/` directory or direct local paths.
Generation reads local files only and writes Rust under Cargo's `OUT_DIR` for attachment with `phoxal::api!();`.
Run `cargo phoxal prepare` before a fresh robot build that selects registry or Git participants.

The service declaration is the sole endpoint authority: Protobuf files carry message definitions only, and authored Protobuf service declarations are rejected.
A robot package reads its local endpoints only from the `robot.yaml` brain section — an adjacent `service.yaml` or `component.yaml` is unrelated to the robot build and is never a fallback source, while an absent or empty brain section generates an empty contract.
YAML type references resolve against the package's own schemas, the built-in robotics vocabulary, and every selected participant's schemas.
Independent packages may carry identical copies of a shared vocabulary; copies collapse during composition and conflicting definitions are rejected explicitly.

It is a build dependency only.
It does not start a runtime or perform transport I/O.
