# Phoxal Framework

The Phoxal robot framework as one coherent workspace: the published `phoxal-bus`
ABI crate (the typed bus client + contract/addressing primitives), the published
`phoxal-api` crate (the dated `api::<version>` contract tree of version-local wire
bodies + topic builders), the published `phoxal` library crate (engine + model)
and its `phoxal-macros` companion, plus a growing set of unpublished platform
runtime binaries (`phoxal-runtime-<name>`) that ship as deployables.
Each crate carries its own version and is released only when it changes; the
library crates are published to crates.io (see [Releasing](#releasing)).

Design docs are in [`docs/`](docs/): [contract discipline](docs/CONTRACTS.md),
[conventions](docs/CONVENTIONS.md), and [validation](docs/VALIDATION.md).

## Releasing

Releases are driven by [release-plz](https://release-plz.dev) with **per-artifact
versions**: each crate carries its own version and is released only when it
changes.

On every push to `main`, release-plz opens or updates a single
`chore(release): release` PR that bumps just the library crates whose code changed
since their last release and refreshes their changelogs.
Merging that PR publishes the changed library crates (`phoxal-bus`, `phoxal-api`,
`phoxal`, `phoxal-macros`) to crates.io at their own versions and tags each
`<crate>-v<version>` with a GitHub release.

The runtime crates (`phoxal-runtime-<name>`) are `publish = false` and are not part
of this crate release; their distribution is handled separately.

See [`.github/workflows/release-plz.yml`](.github/workflows/release-plz.yml) and
[`release-plz.toml`](release-plz.toml).

> Per-target standalone binary tarballs, and the (removed) Webots simulator +
> joypad tool, are not published by this workflow.

## License

AGPL-3.0-only — see [LICENSE](LICENSE) for the full license text.
A commercial license is available — see [COMMERCIAL.md](COMMERCIAL.md) and reach out via <https://phoxal.com>.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). DCO sign-off required on every commit.

## Phoxal

- <https://phoxal.com>
- <https://github.com/phoxal>
