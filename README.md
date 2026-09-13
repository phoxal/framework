# Phoxal Framework

Rust workspace for the Phoxal Framework libraries, execution supervisor, and official runtime participants.

Phoxal is pre-1.0 and evolving.
Visit <https://phoxal.com> for the project vision and public introduction.
The published Rust API is documented at <https://docs.rs/phoxal>.
This repository and its source are the authority for current framework implementation and architecture details.

## Repository

- `phoxal/` - the framework facade and runtime library
- `crates/` and `contracts/` - independently versioned public libraries, proc
  macros, service-owned contracts, project tooling, and test fixtures
- `supervisor/` - the framework execution supervisor
- `services/`, `components/` - official runtime packages
- `simulators/mujoco/` - the independently versioned MuJoCo application kept
  outside the universal framework library
- `tools/cargo-phoxal/` - the registry-aware project and publication command
- `fixture/` - the authored test robot, world, and components (the example robot project is [phoxal/robot-rover](https://github.com/phoxal/robot-rover))

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and contribution requirements.

## Releases

Every published library, contract, executable, and simulator package owns an
independent semantic version in its Cargo manifest.
Changes are planned by release-plz in a reviewable pull request, and unrelated
packages remain unchanged.
The framework does not publish rewritten packages to crates.io.
After the owner change is merged, the exact `cargo package` archive and
checksum follow the reviewed `phoxal` registry admission path in dependency
order.
Registry publication is intentionally separate from release-plz because the
static registry has no Cargo upload API and requires human review of provenance
and ownership records.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

## License

AGPL-3.0-only. See [LICENSE](LICENSE) and [COMMERCIAL.md](COMMERCIAL.md).
