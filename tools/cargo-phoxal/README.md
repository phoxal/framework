# cargo-phoxal

`cargo-phoxal` is the canonical Phoxal source-development command.

The initial tooling foundation supports `cargo phoxal check`, `cargo phoxal build`, and `cargo phoxal test`.

Each command discovers the nearest robot project, validates explicit composition, resolves source packages through the root Cargo graph, and applies the requested Cargo lock and offline policy.

Registry submission, bundle assembly, deployment, and simulation launch are separate follow-up slices.
