# phoxal-bundle

The bundle layout, and plain readers and writers for it.

```text
<bundle>/
├── manifest.json
├── assets/
└── bin/
```

`manifest.json` is one `phoxal_model::manifest::ManifestDocument`: the compiled
robot under the schema tag `phoxal/manifest/v0`. There is no second document
beside it. The expected process set is `brain` plus every key of the robot's
`services` and `components`, which anyone holding the manifest derives, and every
participant reads its own configuration out of the same body. A binary is found
in `bin/` by the id its process was launched under.

`RuntimeBundle::open` parses the manifest and does nothing else - the supervisor
and every participant use the same reader, and a process the manifest never
mentions opens the bundle exactly like one it does. `ParticipantAssets::read`
reads a file below `assets/` by its `AssetId`; that id is already a validated
relative path and `BundlePath` validates the join again, so a read cannot leave
`assets/`.

`BundleWriter::write` takes the manifest, the assets, and a map from
bundle-relative destination to the executable to copy there. It assembles the
bundle in a private sibling directory and renames it onto its final name, so the
target is either absent or a complete bundle; an existing target is refused, and
a failed write removes its own staging directory.

Nothing here hashes anything. Integrity lives in the archive: `phoxal build`
writes `build.phoxal` beside its `build.phoxal.sha256`, `phoxal deploy` ships
both, and `phoxal install` refuses a mismatch. Once a bundle is on disk, what is
on disk is trusted.
