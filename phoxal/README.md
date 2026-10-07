# phoxal

The Phoxal framework runtime, generated service contracts, and public session client live in this crate.

Source preparation, Cargo orchestration, and immutable bundle assembly belong to the independently owned `cargo-phoxal` application.
Native model composition belongs to the independent simulator application.
The runtime consumes the completed bundle manifest and never parses `robot.yaml`, `component.yaml`, or native model source files.

## Consumer profiles

The crate exposes the same owned contract definitions in every profile and one Cargo feature per supported executable role.
Features select compile-time surfaces and dependencies, while process ownership and the constructible API enforce authority at runtime.
The base profile is inert data: contracts, schemas and codecs, geometry helpers, protocol identities, and artifact records, without a runner or transport.

A robot Runtime package uses the default feature set:

```toml
[dependencies]
phoxal = "0.69"
```

An application attaching to a running execution:

```toml
[dependencies]
phoxal = { version = "0.69", default-features = false, features = ["session"] }
```

Scenario authoring belongs to the scenario tier, so a robot selects it as a dev dependency:

```toml
[dev-dependencies]
phoxal = { version = "0.69", features = ["scenario"] }
```

A public contract consumer with no transport or runner takes the base profile:

```toml
[dependencies]
phoxal = { version = "0.69", default-features = false }
```

Prepared-product generation is selected in build dependencies only:

```toml
[build-dependencies]
phoxal = { version = "0.69", default-features = false, features = ["build"] }
```

The `runtime` profile is the default and provides the synchronous Runtime macros, typed inputs and outputs, runner, and required transport.
The `session` profile provides only the public logical-session client and its Protobuf transport.
The `scenario` profile provides scenario authoring and the fixture client without the participant runner or transport.
The `build` profile provides local prepared-product generation for build scripts.
The supervisor executable owns its host implementation privately and consumes the reusable `runtime`, `scenario`, and `session` APIs.

`#[phoxal::message]` and `#[phoxal::endpoints]` define runnable package contracts in Rust.
`phoxal::contracts` contains stable shared robotics, geometry, and component vocabulary; service-specific records remain with their owner.
`phoxal::contracts::robotics::MotionSetpoint` carries planar body-frame forward velocity and counter-clockwise yaw rate independently of the consuming service.
Motion control modes, arm/disarm, authority, limits, and emergency policy belong to the Motion service.
An input payload that crosses a process boundary uses `#[phoxal::message]`, even when it is private to the consumer and needs no authored package or version.
An algorithm-only struct that never crosses that boundary remains ordinary Rust.
Exported payloads and the endpoint struct share one package declaration in `#[phoxal::messages(package = "owner.v1")] mod v1 { ... }`; a provided operation's identity defaults to that package plus its UpperCamelCase field name, while a call always names the provider's operation explicitly.
The battery and lidar vocabularies are schemas only: no capability derives their endpoints yet and the native simulator implements no provider for them.
Enum fields use typed Rust enums with explicit stable discriminants; unknown numeric values and missing payload-enum variants fail decoding.
The framework's bootstrap, session, execution, and simulation protocols use the same Rust message authoring path under `phoxal::communication`.
The runtime metadata attachment and canonical `google.protobuf.Empty` use that same path.
Their runtime construction/validation methods remain in the runtime owner.
These types remain inert in the base profile and keep standard Protobuf wire identities, field numbers, enum discriminants, and presence semantics.
`phoxal::communication::file_descriptor_set()` assembles their standard descriptor set from the same schema records retained in executable artifacts; call Prost's `encode_to_vec()` to export descriptor bytes.
The production SDK and build helper need no protoc or generated protocol source.
Independent `.proto` declarations are test references only.
Unknown enum numbers fail decoding rather than becoming a default value.
Session bootstrap negotiates `phoxal.session.v1.r2`; execution and simulation declare their scoped revision 2.
Wire package identities remain `*.v1`, and artifact formats remain V0.
Optional scalar zero values and empty strings retain presence, absent nested messages remain `None`, and repeated fields remain ordered vectors.
The simulation provider's explicitly numeric `shape` field remains an integer with admission validation; it is not converted to an enum silently.
`phoxal::build::api(BuildApiConfig::default())` reads exact prepared participant products in an ordinary robot build script.
Place `phoxal::api!();` once at a binary or library crate root to attach its generated `api` module.
The helper reads local prepared products only, so prepare selected path or pinned Git participants with `cargo phoxal prepare` before the first bare Cargo build.
The project compiler owns source preparation and project validation inside the `cargo-phoxal` package.
When selected services use different observation payload types, place ordinary `From` or `TryFrom` implementations in the robot's `src/conversions.rs` and declare `mod conversions;`.
The single `phoxal::api!()` attachment derives their bounded input/output endpoints from the prepared provider contracts and the authored graph.
Conversions execute in the brain's normal runtime invocation, preserving the original capture stamp and consumer age bound.
Launch the authored brain with `phoxal::runtime::run::<runtime::Brain>()`.
Brain-owned conversions can also run explicitly in an ordinary step over canonical provider inputs; `#[phoxal::output(stamped, max_bytes = ...)]` accepts a `phoxal::runtime::Sample` so forwarding preserves its original provenance.
Required provider bindings must be prepared before compiling the complete consumer.

A participant can declare a bounded family of leased scalar outputs when its admitted configuration determines the port names.
For example, an output with `family = "/wheels", suffix = "_actuator", max_ports = 8` resolves one typed output for each key of that configuration object.
The projection returns `Vec<(String, Option<Payload>)>` with every configured logical name; `None` withdraws that member's lease.
The SDK resolves and validates the complete per-instance roster, including collisions with fixed ports, before admission.
An invocation with missing, extra, duplicate, oversized, or incompatible members fails before committing any output from that candidate.
This is coordinated invocation acceptance, not a guarantee of atomic multicast over the network.
Preparation keeps one reusable executable template and separate configuration-bound instance contracts; generated APIs consume those resolved contracts.
After changing a configuration that determines ports, run preparation before ordinary Cargo compilation; stale instance products refuse with an actionable diagnostic.


Use <https://docs.rs/phoxal> as the authority for the published Rust API.
Visit <https://phoxal.com> for the project vision and public introduction.
This repository's source and documentation are the authority for implementation, architecture, and compile-time contract details.

## License

AGPL-3.0-only.
A commercial license is available; see the repository root.

## Explicit fixture dependencies

Tests that need an external input dependency can pass a fixture-owned registered adapter to `Harness::with_runtime`.
It uses the same serialized owner, generated endpoint view, complete output admission and explicit-time scheduler as `Harness::new`, including reset and pending-call fencing.
Keep injected input and orchestration inside the test fixture, delegate the real runtime dispatch and projections, and leave participant configuration unchanged.
Production runtime initialization does not inspect ambient test selectors.
