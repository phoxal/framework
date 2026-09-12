# cargo-phoxal

`cargo-phoxal` is the canonical Phoxal source-development command.

The tooling supports `cargo phoxal check`, `cargo phoxal build`, and `cargo phoxal test`.

Each command discovers the nearest robot project, validates explicit composition, resolves source packages through the root Cargo graph, and applies the requested Cargo lock and offline policy.

cargo phoxal build assembles the selected brain and service executables into a
deterministic bundle under Cargo's target directory by default, or at
--output <directory>.
The bundle includes inspectable manifest and provenance records and is
published atomically.

Hardware and simulation process launch are intentionally not exposed by this
slice.
The hardware boundary still needs supervisor process admission over the
assembled bundle and its local identity, while simulation needs the
independent MuJoCo application and its native provenance.
The library retains typed local identity/plan primitives for that integration;
bundle assembly itself never claims process startup, protocol admission,
domain readiness, or physical safety.

Registry submission, artifact installation, and passive package staging remain
separate follow-up slices.
