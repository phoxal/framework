# phoxal-build

Build-time support for package-owned Protobuf contracts.

The package-local entry point is `phoxal::build::api(BuildApiConfig::default())` in an ordinary `build.rs`.
It discovers `api/**/*.proto` and, for a robot package, the exact sources selected by `robot.yaml` under its prepared `.phoxal/` directory or direct local paths.
Generation reads local files only and writes Rust under Cargo's `OUT_DIR` for attachment with `phoxal::api!();`.
Run `cargo phoxal prepare` before a fresh robot build that selects registry or Git participants.

The crate packages the `phoxal/api.proto` modifiers and generates ordinary Prost messages plus inert typed call and observation descriptors from owned Protobuf service declarations.

Contract imports are supplied as the exact `FileDescriptorSet` exported by direct Cargo build dependencies and mapped with Prost `extern_path` entries.
The legacy dependency-descriptor entry points do not locate or compile dependency-owned source trees.
It validates dependency descriptor pools independently, accepts identical transitive diamonds, and rejects conflicting file paths or fully qualified symbols before invoking Protobuf generation.

It is a build dependency only.
It does not start a runtime or perform transport I/O.
