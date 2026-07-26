# Phoxal Framework

Implementation workspace for the Phoxal framework train: public Rust libraries
and the official runtime artifacts that ship with them.

Public product and authoring documentation is published at
<https://phoxal.com>. This repository intentionally keeps only the information
needed to build, test, and contribute to its source.

## Develop

Building the workspace requires Rust and Webots R2025a; see
[CONTRIBUTING.md](CONTRIBUTING.md) for the local setup and DCO requirements.

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
```

Repository tests are deterministic unit and compile-contract checks. Host E2E
validation is performed separately against built artifacts and is not a Cargo
test harness.

The checked-in [`examples/hello-rover`](examples/hello-rover) is a small manual
authoring example, not a test fixture contract.

## License

AGPL-3.0-only. See [LICENSE](LICENSE) and [COMMERCIAL.md](COMMERCIAL.md).
