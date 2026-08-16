# phoxal-model

The canonical, validated Phoxal robot model, and the one document it is
persisted as.

`Robot` is the whole of a compiled robot: its identity and structure, the motion
it may make, the `services` it runs with their configuration, the `components` it
mounts, and the `component_types` behind them with the simulation folded into
each. `robot.component(id)` joins an instance with its type and simulation, so no
consumer joins those maps by hand.

`manifest::ManifestDocument` wraps a `Robot` under the schema tag
`phoxal/manifest/v0`. It lives here rather than in `phoxal-bundle` so the
compiler that writes it and the reader that loads it both reach it through the
crate that owns the body.

Authored project documents belong to `phoxal-manifest`. The bundle layout - where
`manifest.json` sits and how its assets are read - belongs to `phoxal-bundle`.
This crate owns no filesystem layout at all.
