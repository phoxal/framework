# phoxal-bundle

Persisted runtime bundle contract for Phoxal.

`phoxal-manifest` compiles authored YAML/URDF into canonical source facts.
Build tooling combines those facts with selected binaries and participant
metadata to construct one [`RuntimeDocument`]. Runtime participant instances
select reusable executable artifacts, so one staged binary can implement more
than one component-bound process. `BundleWriter` streams those executable
sources into a canonical executable mode and persists the document as
`runtime.json` beside the indexed `assets/` and supervisor-only `bin/` trees.
`RuntimeBundle::open_verified` validates the complete artifact and its
schema, canonical model, participant set, artifact contracts, embedded config
schemas, normalized paths, canonical file modes, file sizes, and SHA-256
digests. `BundleWriter` pins each executable source descriptor, assembles
through descriptor-relative no-follow writes in a private sibling directory,
verifies that candidate completely, and only then publishes with a no-replace
rename syscall; an existing
target (including any symlink below it) is refused, so a failed build cannot
modify an installed bundle or escape its root. Linux uses `renameat2` with
`RENAME_NOREPLACE`; macOS uses `renameatx_np` with `RENAME_EXCL`. Other targets
fail closed rather than falling back to a check-then-rename race.

The reader has no source-parser or catalog dependency. A supervisor or builder
uses `RuntimeBundle::open_verified` to verify the complete installed artifact
once. A participant uses `ParticipantBundle::open` to select one typed
`ParticipantId` record before opening its bus; it verifies its running image
against the selected artifact and checks participant assets lazily rather than
re-hashing unrelated binaries. The process receives one validated
`ParticipantRuntimeInputs` value. Assets are logical and digest checked from
the same owned file descriptor that is consumed. On
Unix, the root directory is pinned first; layout enumeration and every path
component are then descriptor-relative, using `fstatat`/`openat` with
`O_NOFOLLOW`. Targets without a robust native no-follow traversal currently
return `UnsupportedSecureOpen`; they never downgrade to an `lstat`/open
best-effort check. Binaries are never reachable through `ParticipantAssets`, and no trusted
asset pathname API is exposed.
