# Phoxal Framework

Rust workspace for the Phoxal Framework libraries, execution supervisor, and official Runtime services and component drivers.

Phoxal is pre-1.0 and evolving.
Visit <https://phoxal.com> for the project vision and public introduction.
The published Rust API is documented at <https://docs.rs/phoxal>.
This repository and its source are the authority for current framework implementation and architecture details.

## Repository

- `phoxal/` - the framework facade and runtime library
- `crates/` - reusable libraries, proc macros, and project tooling
- `supervisor/` - the framework execution supervisor
- `services/`, `components/` - official services and drivers, with their owned contracts beside private binary implementations
- `phoxal/cargo/` - the registry-aware project and publication command
- `robot-rover` - the maintained example robot project is hosted in the separate [phoxal/robot-rover](https://github.com/phoxal/robot-rover) repository

The native MuJoCo simulator application is owned by [phoxal/simulator](https://github.com/phoxal/simulator); it is not part of this workspace. Hardware-only projects and `cargo-phoxal` build and run without it.

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and contribution requirements.

## Install the developer tool

Install or update the framework-owned `cargo-phoxal` package from the Phoxal registry:

```sh
cargo install cargo-phoxal \
  --index sparse+https://phoxal.github.io/registry/ \
  --version 0.0.0-dev.3 \
  --locked
```

Pre-release tools must be selected explicitly because Cargo does not choose them for a plain `cargo install`.
The current `cargo-phoxal` release is `0.0.0-dev.5`.
It consumes the `0.0.0-dev.1` framework package set.
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
Simulation uses the independent [Phoxal Simulator](https://github.com/phoxal/simulator), while `cargo-phoxal` owns installing MuJoCo, building the matching registry package, and maintaining the user installation.

```sh
cargo phoxal simulation install
cargo phoxal simulation status
```

The installer downloads and verifies the supported MuJoCo distribution, installs the exact simulator release from the Phoxal registry, retains its licenses and provenance, and keeps native files outside robot projects.
Use `cargo phoxal simulation upgrade` to replace the managed installation and `cargo phoxal simulation uninstall` to remove it.
An existing official MuJoCo distribution can be selected explicitly with `--mujoco-distribution <path>`.

For a complete source-based example:

```sh
git clone https://github.com/phoxal/robot-rover.git
cd robot-rover
cargo phoxal check --locked
cargo phoxal simulation scenario list --locked
cargo phoxal simulation scenario run ForwardTurnStop \
  --locked --release
```

## Maintained examples

The complete Runtime rewrite examples and fresh-machine setup recipe are in [examples/runtime-rewrite](examples/runtime-rewrite/README.md).

## Releases

Every published library, contract, executable, and simulator package owns a semantic version in its Cargo manifest.
During the current experimental reset, coherent framework trains use the monotonic `0.0.0-dev.N` sequence and exact internal requirements.
The source commit is recorded in immutable registry provenance instead of being embedded in the version.
Changes are planned by release-plz in a reviewable pull request, and unrelated packages remain unchanged after this reset train.
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
