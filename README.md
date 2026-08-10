# Phoxal Framework

Implementation workspace for the Phoxal framework train: public Rust libraries
and the official runtime artifacts that ship with them.

Public product and authoring documentation is published at
<https://phoxal.com>. This repository intentionally keeps only the information
needed to build, test, and contribute to its source.

## Layout

| Path | Holds |
| --- | --- |
| `phoxal/` | The facade crate, `phoxal`. |
| `crates/<suffix>/` | The library crate `phoxal-<suffix>`. |
| `services/<id>/` | The official service `phoxal/service-<id>`. |
| `components/<id>/` | The official component `phoxal/component-<id>`, driver binary and authored assets in one package. |
| `simulators/<id>/` | The official simulator `phoxal/simulator-<id>`. |
| `fixture/` | Authored YAML and URDF for the workspace fixture robot. No code. |
| `workspace-policy/` | The rules above, enforced as tests under `cargo test --workspace`. |

A directory holds many artifacts and reads plural; a name qualifies exactly one and reads singular.
`crates/` already says `phoxal`, so its children do not repeat it.
Neither convention is a matter of taste here: `workspace-policy` fails the build when a package's directory and its published name stop agreeing.

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
