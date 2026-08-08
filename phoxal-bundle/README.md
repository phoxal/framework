# phoxal-bundle

Persisted runtime bundle contract for Phoxal.

`phoxal-manifest` compiles authored YAML/URDF into canonical source facts.
Build tooling combines those facts with selected binaries and participant
metadata to construct one [`RuntimeDocument`]. `BundleWriter` persists that
document as `runtime.json` beside the indexed `assets/` and supervisor-only
`bin/` trees. `RuntimeBundle` reopens the same artifact and validates its
schema, canonical model, participant set, embedded config schemas, normalized
paths, file sizes, and SHA-256 digests. `BundleWriter` assembles in a private
sibling directory and publishes with one rename; an existing target (including
any symlink below it) is refused, so a failed build cannot modify an installed
bundle or escape its root.

The reader has no source-parser or catalog dependency. A participant selects
one typed `ParticipantId` record before opening its bus and receives one
validated `ParticipantRuntimeInputs` value. Assets are logical and digest
checked from the same owned file descriptor that is returned or consumed. On
Unix, every path component is opened with `openat` and `O_NOFOLLOW`; other
platforms retain the layout's symlink rejection and use the platform's best
available no-follow check. Binaries are never reachable through
`ParticipantAssets`, and no trusted asset pathname API is exposed.
