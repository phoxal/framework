# phoxal-build

Build-time support for Rust-authored contracts and generated client bindings.

The package-local entry point is `phoxal::build::api(BuildApiConfig::default())` in an ordinary `build.rs`.
It reads exact participant products written by `cargo phoxal prepare`, plus local component capability declarations when applicable, and generates Rust under Cargo's `OUT_DIR` for `phoxal::api!();`.
The build helper reads local files only and does not fetch or build another participant.

Services and brains declare their endpoints with Rust macros in their executable packages.
Component capabilities in `component.yaml` provide standard endpoints; component-specific endpoints are authored in Rust.
`robot.yaml` selects participants and connects their ports, and the robot package provides ordinary Rust conversions for differently typed latest observations.

Production messages, including the SDK protocols, are authored with `#[phoxal::message]` or `#[phoxal::messages]`.
Their compiler-resolved schema records provide standard Protobuf descriptors.
The build helper consumes prepared descriptors and never invokes protoc.
Independent Protobuf reference compilation belongs only to the schema-proof test fixture.

Prepared contract publication and reading share the `phoxal-build` boundary.
`write_prepared` receives source selection, executable provenance, runtime metadata, and standard descriptors.
It owns serialization, a stable publication lock, coherent pair replacement, and interrupted-publication recovery.
Unchanged contract and descriptor content preserves the prepared files even after an implementation-only rebuild.
