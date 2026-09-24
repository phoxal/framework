# cargo-phoxal

`cargo-phoxal` is the canonical Phoxal source-development command.

It is a separately versioned Cargo package owned by the framework repository.
The tool stays beside the SDK, artifact format, scenario protocol, and supervisor contracts it compiles against so one reviewed change can keep those private boundaries coherent.

## Installation

Install or update the released package from the Phoxal registry:

```sh
cargo install cargo-phoxal \
  --index sparse+https://phoxal.github.io/registry/ \
  --version 0.0.0-dev.6 \
  --locked
```

Cargo requires an explicit version when installing a pre-release.
The current `cargo-phoxal` release is `0.0.0-dev.6`.
It consumes the `0.0.0-dev.2` framework package set.

Cargo exposes the installed binary as `cargo phoxal`.
Publishing a new tool version requires its exact `phoxal` dependency to be available in the registry first.
The complete owner-first order begins with `phoxal-build`, `phoxal-macros`, and `phoxal`, then continues through services, components, the supervisor, and `cargo-phoxal`.

For framework development before those packages are released, run the workspace binary explicitly:

```sh
cargo run --locked -p cargo-phoxal -- phoxal --help
```

The tooling supports `cargo phoxal prepare`, `cargo phoxal check`, `cargo phoxal build`, `cargo phoxal run`, `cargo phoxal test`, managed simulation installation and execution, and reviewed package publication.

`cargo phoxal prepare` resolves each service and component from its required `source` selection in `robot.yaml`.
Choose exactly one source form:

```yaml
source: { path: ../../components/ddsm115 }
source: { package: { name: phoxal-component-ddsm115, version: "1.2.3" } }
source: { package: { name: phoxal-component-ddsm115, version: "1.2.3", registry: other } }
source:
  git:
    name: phoxal-component-ddsm115
    url: https://example.com/components.git
    rev: 0123456789abcdef0123456789abcdef01234567
    path: ddsm115
```

Local paths are relative to the robot root and use the selected Cargo package's own version.
Registry selections require an exact package name and semantic version; omitting `registry` selects the Phoxal registry.
Git selections require a package name and full commit revision, with an optional package path below the checkout; Cargo reads that package's version from the pinned checkout.
Registry and Git runnable packages are installed into the managed Phoxal home.
It copies registry and Git package Protobuf sources into the project's ignored `.phoxal/` tree, while local path sources remain at their authored paths.
Managed installations retain the required API and component model resources, not another copy of the package source tree.
Local path participants are built by Cargo in their own workspace when a runtime bundle is needed.
Preparation preserves the authored selection and leaves the robot's Cargo manifest unchanged.
The check, build, run, test, bundle, and simulation entry points prepare these exact selections automatically.

Each command discovers the nearest robot project, validates explicit composition, and applies the requested Cargo lock and offline policy.
Registry and Git runnable participant packages install through Cargo into the managed Phoxal home outside the robot's dependency graph.
Only the robot application, supervisor, and passive data packages remain in its root Cargo graph.

`cargo phoxal check` validates the authored document, prepares selected APIs, and runs Cargo check for the requested robot code.
It does not build runtime executables or inspect their compiled contracts.

`cargo phoxal build` assembles the selected brain, service and component-driver executables, and mandatory supervisor into a deterministic bundle under Cargo's target directory by default, or at `--output <directory>`.
The bundle contains the selected executables, the full compiled `robot.yaml`, and a manifest that maps service instances to executable paths and retains the runtime contracts needed for admission.
Simulation bundles also contain the model assets needed by the simulator.
Bundle files carry no checksum or provenance records.

`cargo phoxal run` independently prepares and validates the hardware bundle, then launches the selected supervisor with the isolated `local` scope and `local` supervisor identity.
It does not launch simulation or claim domain readiness or physical safety.

Change a registry participant's version or a Git participant's revision in `robot.yaml`, then run preparation or build to acquire it.
Local path packages use their current checked-out content.

All source-development commands accept `--cargo <path>` and preserve the selected executable across Cargo metadata and operation invocations.
Cargo package, workspace, target, and test selectors are forwarded using Cargo's native option names.
In JSON compiler-message mode, compiler JSON remains on stdout and Phoxal progress and structured project diagnostics remain on stderr.

The root project declares its ordinary `phoxal` SDK and supervisor dependencies.
Preparation never adds per-participant Cargo dependencies or rewrites `Cargo.toml`.
Every selected runtime executable must expose its exact compiled contract metadata, and authored configuration is checked against that metadata before a bundle is published.

## Simulation installation

Install the supported MuJoCo distribution and the matching released simulator with:

```sh
cargo phoxal simulation install
cargo phoxal simulation status
```

The installer verifies the official MuJoCo archive checksum, builds the exact `phoxal-simulator` registry package, preserves its standalone Cargo graph and native licenses, and writes one managed user installation.
On macOS the result is a self-contained locally signed application bundle.
On Linux the executable is linked to the managed native distribution with an explicit runtime search path.
Robot manifests never depend on MuJoCo or the simulator.

Use `cargo phoxal simulation upgrade` to replace the managed installation and `cargo phoxal simulation uninstall` to remove only the directory marked as owned by `cargo-phoxal`.
Use `--mujoco-distribution <path>` when an official MuJoCo distribution is already available, or together with `--offline` for an installation that performs no download.
An explicit `--simulator <path>` remains available for simulator source development and deterministic test fixtures.

`cargo phoxal publish <role> <name> --dry-run` selects an exact local Cargo package and produces a `.crate` archive in isolated temporary staging.
The command prints the archive checksum and size without creating a review inventory, source provenance record, or checksum sidecar.
Supported roles are `component`, `service`, `preset`, `library`, `proc-macro`, `simulator`, `application`, and `tool`.

The developer selects the publication role in the command instead of repeating it in package metadata.
`cargo-phoxal` verifies that selection from standard project structure: `component.yaml` identifies a component, `service.yaml` identifies a service preset, and Cargo target shape distinguishes ordinary libraries, procedural macros, applications, simulators, and tools.
Runtime composition derives service and component roles from `robot.yaml` source selections.
No Phoxal-specific package metadata table is required.

The optional `--path <source-directory>` selects a package explicitly, while omitting it selects the matching current package or a uniquely named member of the current Cargo workspace.

Passive components may contain only authored Cargo package metadata, a `component.yaml`, and declared assets.
The publication preparer adds `_cargo/lib.rs` and technical Cargo packaging fields only in staging, and never writes generated files into the authored source tree.

Omit `--dry-run` to submit those exact bytes to `phoxal/registry` for review.
Submission uses GitHub HTTPS APIs directly and invokes neither Git nor GitHub CLI.
Set `PHOXAL_GITHUB_TOKEN` for an explicit noninteractive credential, keeping it outside command arguments and submitted content.
Otherwise the released tool uses its embedded public OAuth client ID, obtains the `public_repo` scope through GitHub's bounded device flow, validates the authenticated account, and stores renewable credentials in the operating-system credential store.
Development builds may provide the public client ID through `PHOXAL_GITHUB_CLIENT_ID`.
The `public_repo` scope covers every public repository accessible to the account, not only the registry fork.
The command creates or reuses a contributor fork and immutable publication branch, uploads the archive and Cargo index bytes through Git objects, opens or reuses the upstream pull request, and returns `pending-review` without waiting for merge.
An already deployed version is reported as `available` only after its public archive checksum is verified.
