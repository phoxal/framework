# Phoxal Framework

The Phoxal robot framework as one coherent workspace: the published `phoxal-bus`
ABI crate (the typed bus client + contract/addressing primitives), the published
`phoxal-api` crate (the dated `api::<version>` contract tree of version-local wire
bodies + topic builders), the published `phoxal` library crate (engine + model)
and its `phoxal-macros` companion, plus a growing set of unpublished platform
service binaries (`phoxal-service-<name>`) that ship as deployables.
Each crate carries its own version and is released only when it changes; the
library crates are published to crates.io (see [Releasing](#releasing)).

Design docs are in [`docs/`](docs/): [contract discipline](docs/CONTRACTS.md),
[conventions](docs/CONVENTIONS.md), and [validation](docs/VALIDATION.md).

## Getting started

Robot projects are hand-authored: a `robot.yaml` manifest, a `structure.urdf`,
component definitions, and user service crates.
[`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md) walks through authoring
one using the checked-in [`examples/hello-rover`](examples/hello-rover)
project, and links the editor-facing
[`examples/robot.schema.json`](examples/robot.schema.json) JSON Schema for
`robot.yaml`.

## Releasing

Each crate carries its own version and advances only when its own source
changes. The 4 library crates and the official artifact crates use different
version authorities.

[release-plz](https://release-plz.dev) owns only the libraries (`phoxal-bus`,
`phoxal-api`, `phoxal`, `phoxal-macros`). On a schedule (and on demand via
workflow dispatch), it opens or updates a single `chore(release): release` PR
for their version and changelog changes.
Merging that PR publishes the changed library crates (`phoxal-bus`,
`phoxal-api`, `phoxal`, `phoxal-macros`) to crates.io. The registry is their
only distribution and version-baseline channel; they create no per-library git
tags or GitHub releases.

The official artifact crates (`phoxal-service-<name>`, component crates,
`phoxal-tool-<name>`, `phoxal-simulator-<name>`) set `publish = false`, which
keeps them entirely outside release-plz. `cargo xtask release bump --changed`
compares each artifact's own crate directory with its current
`{package}-v{version}` tag and adds only the changed artifacts' patch bumps to
the same release PR. Workspace lockfile and library changes cannot select an
artifact. After merge, `cargo xtask release cut` creates the missing artifact
tags and emits the JSON handoff consumed by `cargo xtask release plan`.

Building and publishing the artifact binaries is decoupled from versioning. A
release that publishes any of the four shared libraries rebuilds every official
artifact because their linked code may have changed. An artifact-only release
builds and uploads only the artifacts whose independent versions were newly
tagged. A forced recovery build, cold start, or non-coherent previous latest
catalog also rebuilds the complete set.

Each wave uploads its selected archives together with an assembled
`phoxal.catalog/v0` `catalog.json` full index to one immutable
`build-YYYYMMDD-<run>` GitHub release, which is marked "latest" once the gate
passes. The catalog carries unchanged artifact versions forward with their
existing permanent download URLs. Catalog assembly cross-target-validates the
changed binaries' embedded metadata, including their `config_schema` slot; the
release-PR gate checks coherence across the complete official participant set.
A pinned artifact version resolves through the catalog to its permanent
download URL.

See [`.github/workflows/release-plz.yml`](.github/workflows/release-plz.yml) and
[`release-plz.toml`](release-plz.toml).

## License

AGPL-3.0-only - see [LICENSE](LICENSE) for the full license text.
A commercial license is available - see [COMMERCIAL.md](COMMERCIAL.md) and reach out via <https://phoxal.com>.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). DCO sign-off required on every commit.

## Phoxal

- <https://phoxal.com>
- <https://github.com/phoxal>
