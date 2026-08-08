# phoxal-bundle

Persisted runtime bundle contract for Phoxal.

`phoxal-manifest` compiles authored YAML/URDF into canonical source facts.
Build tooling combines those facts with selected binaries and participant
metadata to construct one [`RuntimeDocument`]. `BundleWriter` persists that
document as `runtime.json` beside the indexed `assets/` and supervisor-only
`bin/` trees. `RuntimeBundle` reopens the same artifact and validates its
schema, canonical model, participant set, embedded config schemas, normalized
paths, file sizes, and SHA-256 digests.

The reader has no source-parser or catalog dependency. A participant selects
one typed `ParticipantId` record before opening its bus and receives one
validated `ParticipantRuntimeInputs` value. Assets are logical and digest
checked; binaries are never reachable through `ParticipantAssets`.
