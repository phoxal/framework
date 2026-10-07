# phoxal-build

Build-time support for Rust-authored contracts and generated client bindings.

The package-local entry point is `phoxal::build::api(BuildApiConfig::default())` in an ordinary `build.rs`.
It reads exact participant products written by `cargo phoxal prepare`, plus local component capability declarations when applicable, and generates Rust under Cargo's `OUT_DIR` for `phoxal::api!();`.
The build helper reads local files only and does not fetch or build another participant.

Services and brains declare their endpoints with Rust macros in their executable packages.
Component capabilities in `component.yaml` provide standard endpoints; component-specific endpoints are authored in Rust.
The tool resolves selected robot files into one strict composition with consumer-owned bindings, and the robot package provides ordinary Rust conversions for differently typed latest observations.

Production messages, including the SDK protocols, are authored with `#[phoxal::message]` or `#[phoxal::messages]`.
Their compiler-resolved schema records provide standard Protobuf descriptors.
The build helper consumes prepared descriptors and never invokes protoc.
Independent Protobuf reference compilation belongs only to the schema-proof test fixture.

Prepared contract publication and reading share the `phoxal-build` boundary.
`write_prepared` receives source selection, executable provenance, runtime metadata, and standard descriptors.
It owns serialization, a stable publication lock, coherent pair replacement, and interrupted-publication recovery.
Unchanged contract and descriptor content preserves the prepared files even after an implementation-only rebuild.

Prepared inputs are stored under `<normal Cargo target root>/phoxal/prepared/`.
The CLI writer and ordinary Cargo/build-script/IDE reader use the same local Cargo configuration and environment resolver.
`CARGO_TARGET_DIR`, `CARGO_BUILD_TARGET_DIR`, and hierarchical Cargo `build.target-dir` configuration select this root; absent configuration, it is the robot's `target/` directory.
One-off `--target-dir` and command-line `--config` overrides relocate compiler and runnable outputs only, not prepared inputs.
After changing normal configuration or environment, run `cargo phoxal prepare` again at the new location.
Existing project-side `.cargo` directories are watched for config additions, legacy filename precedence, edits, and deletion.
Environment changes and existing global Cargo config files are also tracked; Cargo's global cache tree is not recursively watched.
Creating a previously absent configuration directory while reusing a compiler-output cache requires one-time recovery:

```sh
cargo phoxal prepare
cargo clean -p <ROBOT_PACKAGE> --target-dir <AFFECTED_COMPILER_OUTPUT>
```

Use the affected output directory, including matching `--target <TRIPLE>` when the original build used one.
Omit `--target-dir` when that output directory is already selected by normal configuration or environment.
Clean only the robot package, then rebuild normally; subsequent unchanged builds remain fresh.
Adding a previously absent global config file also needs this recovery because watching Cargo home would scan its mutable caches.
A missing prepared selection reports the input-store path and that preparation command.
Build scripts do not invoke Cargo or guess paths from `OUT_DIR`.

Robot API generation consumes the exact tool-prepared resolved composition and its immutable contract/descriptor snapshot.
Brain and services are inside the robot section alongside components; all source selections are concrete because the tool has already resolved named references.
It never rereads robot.yaml or implements layer composition.
Selected authored files are watched and checked for freshness; edits require cargo phoxal prepare again.
Each tool command chooses its own file selection, while ordinary Cargo reads the last successfully prepared composition.
Atomic publication selects a coherent snapshot, so concurrent ordinary reads cannot combine different compositions.
