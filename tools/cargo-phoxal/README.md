# cargo-phoxal

`cargo-phoxal` is the canonical Phoxal source-development command.

The tooling supports `cargo phoxal check`, `cargo phoxal build`, `cargo phoxal test`, and local publication preparation.

Each command discovers the nearest robot project, validates explicit composition, resolves source packages through the root Cargo graph, and applies the requested Cargo lock and offline policy.

cargo phoxal build assembles the selected brain and service executables into a deterministic bundle under Cargo's target directory by default, or at `--output <directory>`.
The bundle includes inspectable manifest and provenance records and is published atomically.

Hardware and simulation process launch are intentionally not exposed by this slice.
The hardware boundary still needs supervisor process admission over the assembled bundle and its local identity, while simulation needs the independent MuJoCo application and its native provenance.
The library retains typed local identity/plan primitives for that integration; bundle assembly itself never claims process startup, protocol admission, domain readiness, or physical safety.

Registry submission and artifact installation remain separate follow-up slices.

`cargo phoxal publish component <name> --dry-run` and `cargo phoxal publish service <name> --dry-run` select an exact local Cargo package and produce a verified `.crate` archive, review inventory, and SHA-256 sidecar in isolated temporary staging.

The optional `--path <source-directory>` selects a package explicitly, while omitting it selects the matching current package or a uniquely named member of the current Cargo workspace.

Passive components may contain only authored Cargo package metadata, a `component.yaml`, and declared assets.
The publication preparer adds `_cargo/lib.rs` and technical Cargo packaging fields only in staging, and never writes generated files into the authored source tree.

Remote GitHub authorization, fork, branch, and pull-request submission are not exposed until their complete implementation is available, so publication commands currently require `--dry-run`.
