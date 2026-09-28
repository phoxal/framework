# phoxal-build

Build-time support for Rust-authored contracts and generated client bindings.

The package-local entry point is `phoxal::build::api(BuildApiConfig::default())` in an ordinary `build.rs`.
It reads exact participant products written by `cargo phoxal prepare`, plus local component capability declarations when applicable, and generates Rust under Cargo's `OUT_DIR` for `phoxal::api!();`.
The build helper reads local files only and does not fetch or build another participant.

Services and brains declare their endpoints with Rust macros in their executable packages.
Component capabilities in `component.yaml` provide standard endpoints; component-specific endpoints are authored in Rust.
`robot.yaml` selects participants and connects their ports, and the robot package provides ordinary Rust conversions for differently typed latest observations.

The Protobuf compiler helpers remain for the SDK's own protocol packages and intentional schema conformance proofs.
They are not the authoring path for service endpoints.
