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
schemas, normalized paths, file sizes, and SHA-256 digests. `BundleWriter`
assembles the bundle in a private sibling directory, verifies that candidate
completely, and only then renames it onto its final name, so a target name is
either absent or holds a complete bundle. An existing target is refused, so a
build never modifies an installed bundle. A failed build removes its own
staging directory.

The reader has no source-parser or catalog dependency. A supervisor or builder
uses `RuntimeBundle::open_verified` to verify the complete installed artifact
once: the layout admits exactly `runtime.json`, the indexed files below
`assets/` and `bin/`, and the directories those files need, and every indexed
file is proven by size and digest as it is read. A participant uses
`ParticipantBundle::open` to select one typed `ParticipantId` record before
opening its bus; it checks participant assets lazily rather than re-hashing
unrelated binaries, and receives one validated `ParticipantRuntimeInputs`
value. Assets are logical and digest checked as they are read. Binaries are
never reachable through `ParticipantAssets`, and no asset pathname API is
exposed.
