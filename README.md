# Phoxal Framework

Rust workspace for the Phoxal Framework libraries, execution supervisor, and
official runtime participants.

Phoxal is pre-1.0 and evolving. Public product and authoring documentation is
published at <https://phoxal.com>.

## Repository

- `phoxal/` — robot-authoring facade
- `crates/` — framework libraries
- `supervisor/` — framework-train execution supervisor
- `services/`, `components/`, `simulators/` — official runtime packages
- `examples/` and `fixture/` — development examples and test inputs

See [CONTRIBUTING.md](CONTRIBUTING.md) for setup and contribution requirements.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace --all-targets
```

## License

AGPL-3.0-only. See [LICENSE](LICENSE) and [COMMERCIAL.md](COMMERCIAL.md).
