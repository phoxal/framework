# Phoxal Framework

Rust workspace for the Phoxal Framework libraries, execution supervisor, and official Runtime services and component drivers.

Phoxal is pre-1.0 and evolving.
Visit <https://phoxal.com> for the project vision and public introduction.
The published Rust API is documented at <https://docs.rs/phoxal>.
This repository and its source are the authority for current framework implementation and architecture details.

## Repository

- `phoxal/` - the framework facade and runtime library
- `supervisor/` - the framework execution supervisor
- `services/`, `components/` - official services and drivers, with their owned contracts beside private binary implementations
- `phoxal/cargo/` - the registry-aware project and publication command
- `tests/` - internal compile-contract fixtures and one four-wheel native qualification robot

Hardware-only projects and `cargo-phoxal` build and run without MuJoCo or a simulator installation.

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and contribution requirements.

## Install the developer tool

Install or update the framework-owned `cargo-phoxal` package from the Phoxal registry:

```sh
cargo install cargo-phoxal \
  --index sparse+https://phoxal.github.io/registry/ \
  --version 0.0.0-dev.6 \
  --locked
```

Pre-release tools must be selected explicitly because Cargo does not choose them for a plain `cargo install`.
The current `cargo-phoxal` release is `0.0.0-dev.6`.
It consumes the `0.0.0-dev.2` framework package set.
Later incompatible development trains increment only the final pre-release counter.
The release order begins with `phoxal-build`, `phoxal-macros`, and `phoxal`, then continues through services, components, the supervisor, and `cargo-phoxal`.
The build helper is required first because `phoxal` exposes the same Protobuf authoring implementation to consumer build scripts while also using it for its own generated contracts.

The installed executable is invoked as a Cargo subcommand:

```sh
cargo phoxal --help
cargo phoxal check --locked
```

`cargo-phoxal` is versioned and published independently, but it remains in this repository because it directly owns the project compiler, bundle format integration, scenario protocol, and supervisor launch contract.
Keeping those coupled changes in one owner repository prevents version-skewed adapters and duplicate project models.

Hardware-only development does not require MuJoCo.
Simulation uses the separately installed Phoxal Simulator application, while `cargo-phoxal` owns installing MuJoCo, building the matching registry package, and maintaining the user installation.

```sh
cargo phoxal simulation install
cargo phoxal simulation status
```

The installer downloads and verifies the supported MuJoCo distribution, installs the exact simulator release from the Phoxal registry, retains its licenses and provenance, and keeps native files outside robot projects.
Use `cargo phoxal simulation upgrade` to replace the managed installation and `cargo phoxal simulation uninstall` to remove it.
An existing official MuJoCo distribution can be selected explicitly with `--mujoco-distribution <path>`.

The repository intentionally has no maintained user examples while its pre-1.0 authoring and runtime contracts are still changing.
Before pushing a change that can affect project preparation, runtime transport, components, or simulation, use the internal four-wheel robot in the root Cargo workspace.
It contains only four actuator/encoder components, a disarmed brain, a small robot-local controller, and the finite `ForwardTurnStop` scenario.

Build the current framework tool and a source simulator as documented by the simulator repository, then run:

```sh
cd tests/robot
../../../target/debug/cargo-phoxal check --locked
../../../target/debug/cargo-phoxal simulation scenario list --locked
../../../target/debug/cargo-phoxal simulation scenario run ForwardTurnStop \
  --locked --release --headless \
  --simulator ../../../../simulator/target/release/phoxal-simulator
```

This native qualification is a local pre-push gate for now.
It is not a public example, a supported robot template, or a substitute for focused deterministic tests.

## Releases

Every published library, contract, executable, and simulator package owns a semantic version in its Cargo manifest.
During the current experimental reset, coherent framework trains use the monotonic `0.0.0-dev.N` sequence and exact internal requirements.
The source commit is recorded in immutable registry provenance instead of being embedded in the version.
When a release train is intentionally started, release-plz plans changes in a reviewable pull request, and unrelated packages remain unchanged after this reset train.
Release planning is manually dispatched because the static reviewed registry has no publication API or release state that release-plz can observe.
The framework does not publish rewritten packages to crates.io.
After the owner change is merged, the exact `cargo package` archive and checksum follow the reviewed `phoxal` registry admission path in dependency order.
Registry publication is intentionally separate from release-plz because the static registry has no Cargo upload API and requires human review of provenance and ownership records.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

## License

AGPL-3.0-only. See [LICENSE](LICENSE) and [COMMERCIAL.md](COMMERCIAL.md).
