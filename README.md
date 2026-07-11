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

## Releasing

Each crate carries its own version. [release-plz](https://release-plz.dev) owns
versioning for *every* crate in the workspace - the 4 library crates and the
official artifact crates alike. Libraries release when their packaged source
changes; every artifact advances on each release train so the catalog carries a
fresh coherent binary set.

On a schedule (and on demand via workflow dispatch), release-plz opens or
updates a single `chore(release): release` PR that bumps every crate whose code
changed since its last release and refreshes its changelog.
Merging that PR publishes the changed library crates (`phoxal-bus`,
`phoxal-api`, `phoxal`, `phoxal-macros`) to crates.io. The registry is their
only distribution and version-baseline channel; they create no per-library git
tags or GitHub releases.
The official artifact crates (`phoxal-service-<name>`, component drivers,
`phoxal-tool-<name>`, `phoxal-simulator-<name>`) keep `publish = false` in their
own `Cargo.toml`; release-plz versions them via `git_only` (a git-tag version
ledger, never crates.io) and creates no per-artifact GitHub release.

Building and publishing the artifact binaries is decoupled from versioning.
On each push to `main`, the release workflow computes which artifact versions
are not yet in the catalog, builds only those, and uploads them - together with
an assembled `catalog.json` full index - to a single immutable
`build-YYYYMMDD-<run>` GitHub release, which is marked "latest" once the gate
passes.
A pinned artifact version resolves through that one catalog to its permanent
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
