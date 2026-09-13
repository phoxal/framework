# cargo-phoxal

`cargo-phoxal` is the canonical Phoxal source-development command.

The tooling supports `cargo phoxal check`, `cargo phoxal build`, `cargo phoxal run`, `cargo phoxal test`, and reviewed package publication.

Each command discovers the nearest robot project, validates explicit composition, resolves source packages through the root Cargo graph, and applies the requested Cargo lock and offline policy.

`cargo phoxal build` assembles the selected brain, service, and component-driver executables and the mandatory supervisor through the root Cargo graph into a deterministic bundle under Cargo's target directory by default, or at `--output <directory>`.
The bundle includes inspectable manifest and provenance records and is published atomically.

`cargo phoxal run` independently prepares and validates the hardware bundle, then launches the selected supervisor with the isolated `local` scope and `local` supervisor identity.
It does not launch simulation or claim domain readiness or physical safety.

Ordinary preparation adds the known `phoxal-supervisor` dependency with an unconstrained first-resolution requirement when it is missing, preserving existing manifest content and letting Cargo select the newest compatible package.
`--locked` and `--frozen` report an actionable initialization error before changing `Cargo.toml` or `Cargo.lock`.
Every selected runtime executable must expose its exact compiled contract metadata, and authored configuration is checked against that metadata before a bundle is published.

`cargo phoxal publish component <name> --dry-run` and `cargo phoxal publish service <name> --dry-run` select an exact local Cargo package and produce a verified `.crate` archive, review inventory, and SHA-256 sidecar in isolated temporary staging.

The optional `--path <source-directory>` selects a package explicitly, while omitting it selects the matching current package or a uniquely named member of the current Cargo workspace.

Passive components may contain only authored Cargo package metadata, a `component.yaml`, and declared assets.
The publication preparer adds `_cargo/lib.rs` and technical Cargo packaging fields only in staging, and never writes generated files into the authored source tree.

Omit `--dry-run` to submit those exact bytes to `phoxal/registry` for review.
Submission uses GitHub HTTPS APIs directly and invokes neither Git nor GitHub CLI.
Set `PHOXAL_GITHUB_TOKEN` for an explicit noninteractive credential, keeping it outside command arguments and submitted content.
Otherwise the released tool uses its embedded public OAuth client ID, obtains the `public_repo` scope through GitHub's bounded device flow, validates the authenticated account, and stores renewable credentials in the operating-system credential store.
Development builds may provide the public client ID through `PHOXAL_GITHUB_CLIENT_ID`.
The `public_repo` scope covers every public repository accessible to the account, not only the registry fork.
The command creates or reuses a contributor fork and immutable publication branch, uploads the archive/index/provenance/ownership bytes through Git objects, opens or reuses the upstream pull request, and returns `pending-review` without waiting for merge.
An already deployed version is reported as `available` only after its public archive checksum is verified.
