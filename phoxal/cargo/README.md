# cargo-phoxal

`cargo-phoxal` is the canonical Phoxal source-development command.

It is a separately versioned Cargo package owned by the framework repository.
The tool stays beside the SDK, artifact format, scenario protocol, and supervisor contracts it compiles against so one reviewed change can keep those private boundaries coherent.

## Installation

Install or update the released package from the Phoxal registry:

```sh
cargo install cargo-phoxal \
  --index sparse+https://phoxal.github.io/registry/ \
  --version 0.0.0-dev.3 \
  --locked
```

Cargo requires an explicit version when installing a pre-release.
The current `cargo-phoxal` release is `0.0.0-dev.3`.
It consumes the `0.0.0-dev.1` framework package set.

Cargo exposes the installed binary as `cargo phoxal`.
Publishing a new tool version requires its exact `phoxal` dependency to be available in the registry first.
The complete owner-first order begins with `phoxal-build`, `phoxal-macros`, and `phoxal`, then continues through services, components, the supervisor, and `cargo-phoxal`.

For framework development before those packages are released, run the workspace binary explicitly:

```sh
cargo run --locked -p cargo-phoxal -- phoxal --help
```

The tooling supports `cargo phoxal check`, `cargo phoxal build`, `cargo phoxal run`, `cargo phoxal test`, `cargo phoxal update`, managed simulation installation and execution, and reviewed package publication.

Each command discovers the nearest robot project, validates explicit composition, resolves source packages through the root Cargo graph, and applies the requested Cargo lock and offline policy.

`cargo phoxal build` assembles the selected brain, service, and component-driver executables and the mandatory supervisor through the root Cargo graph into a deterministic bundle under Cargo's target directory by default, or at `--output <directory>`.
The bundle includes inspectable manifest and provenance records and is published atomically.

`cargo phoxal run` independently prepares and validates the hardware bundle, then launches the selected supervisor with the isolated `local` scope and `local` supervisor identity.
It does not launch simulation or claim domain readiness or physical safety.

`cargo phoxal update` runs the requested Cargo update against the owning workspace lock and then performs a fresh Phoxal preparation, exact contract extraction, configuration validation, and connection validation before reporting success.
Update-only Cargo arguments are not replayed into the validation builds.

All source-development commands accept `--cargo <path>` and preserve the selected executable across Cargo metadata and operation invocations.
Cargo package, workspace, target, and test selectors are forwarded using Cargo's native option names.
In JSON compiler-message mode, compiler JSON remains on stdout and Phoxal progress and structured project diagnostics remain on stderr.

Ordinary preparation adds the known `phoxal-supervisor` dependency with an unconstrained first-resolution requirement when it is missing, preserving existing manifest content and letting Cargo select the newest compatible package.
`--locked` and `--frozen` report an actionable initialization error before changing `Cargo.toml` or `Cargo.lock`.
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

`cargo phoxal publish <role> <name> --dry-run` selects an exact local Cargo package and produces a verified `.crate` archive, review inventory, and SHA-256 sidecar in isolated temporary staging.
Supported roles are `component`, `service`, `preset`, `library`, `proc-macro`, `simulator`, `application`, and `tool`.

The developer selects the publication role in the command instead of repeating it in `[package.metadata.phoxal]`.
`cargo-phoxal` verifies that selection from standard project structure: `component.yaml` identifies a component, `service.yaml` identifies a service preset, and Cargo target shape distinguishes ordinary libraries, procedural macros, applications, simulators, and tools.
Runtime composition similarly derives service and component roles from `robot.yaml` dependency selection.
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
The command creates or reuses a contributor fork and immutable publication branch, uploads the archive/index/provenance/ownership bytes through Git objects, opens or reuses the upstream pull request, and returns `pending-review` without waiting for merge.
An already deployed version is reported as `available` only after its public archive checksum is verified.
