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

Each crate carries its own version and is released only when it changes, but the
4 library crates and the official artifact crates are released by different
mechanisms.

[release-plz](https://release-plz.dev) owns the library crates only
(`phoxal-bus`, `phoxal-api`, `phoxal`, `phoxal-macros`).
On a schedule (and on demand via workflow dispatch), release-plz opens or
updates a single `chore(release): release` PR that bumps just the library
crates whose code changed since their last release and refreshes their
changelogs.
Merging that PR publishes the changed library crates to crates.io at their own
versions and tags each `<crate>-v<version>` with a GitHub release.

The official artifact crates (`phoxal-service-<name>`, `phoxal-driver-<name>`,
`phoxal-tool-<name>`, and `phoxal-simulator-<name>`) set `publish = false` in
their own `Cargo.toml`, so release-plz drops them before its own config ever
applies - they are entirely outside release-plz's scope. Instead, `cargo xtask
release cut` tags and drafts a GitHub release for each artifact whose current
Cargo.toml version isn't tagged yet, and `cargo xtask release bump --changed`
computes their version bumps from git diff. Both run in the same workflow as
release-plz.

See [`.github/workflows/release-plz.yml`](.github/workflows/release-plz.yml) and
[`release-plz.toml`](release-plz.toml).

> Per-target standalone binary tarballs are uploaded to the git-only artifact
> releases created by this workflow.

## License

AGPL-3.0-only - see [LICENSE](LICENSE) for the full license text.
A commercial license is available - see [COMMERCIAL.md](COMMERCIAL.md) and reach out via <https://phoxal.com>.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). DCO sign-off required on every commit.

## Phoxal

- <https://phoxal.com>
- <https://github.com/phoxal>
