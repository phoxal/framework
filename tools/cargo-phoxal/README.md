# cargo-phoxal

`cargo-phoxal` is the canonical Phoxal source-development command.

The tooling supports `cargo phoxal check`, `cargo phoxal build`, and `cargo phoxal test`.

Each command discovers the nearest robot project, validates explicit composition, resolves source packages through the root Cargo graph, and applies the requested Cargo lock and offline policy.

cargo phoxal build assembles the selected brain and service executables into a
deterministic bundle under Cargo's target directory by default, or at
--output <directory>.
The bundle includes inspectable manifest and provenance records and is
published atomically.

cargo phoxal run and cargo phoxal simulation run independently prepare and
build that bundle with the isolated local/local identity by default.
They currently stop with an explicit launch-unavailable diagnostic because
supervisor process admission and independent simulator provisioning are not
owned by this source compiler slice.
They never report a process as Ready merely because Cargo built it.

Registry submission, artifact installation, and passive package staging remain
separate follow-up slices.
