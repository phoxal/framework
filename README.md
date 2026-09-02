# Phoxal Framework

Rust workspace for the Phoxal Framework libraries, execution supervisor, and official runtime participants.

Phoxal is pre-1.0 and evolving.
Visit <https://phoxal.com> for the project vision and public introduction.
The published Rust API is documented at <https://docs.rs/phoxal>.
This repository and its source are the authority for current framework implementation and architecture details.

## Repository

- `phoxal/` — the one framework library
- `crates/` — the proc-macro package and the test fixture stager
- `supervisor/` — framework-train execution observer
- `services/`, `components/` — official runtime packages
- `fixture/` — the authored test robot and components (the example robot project is [phoxal/robot-rover](https://github.com/phoxal/robot-rover))

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and contribution requirements.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

## License

AGPL-3.0-only. See [LICENSE](LICENSE) and [COMMERCIAL.md](COMMERCIAL.md).
