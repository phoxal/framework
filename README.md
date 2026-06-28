# Phoxal Framework

The Phoxal robot framework as one coherent workspace: the published `phoxal`
library crate (engine, model, typed bus, the dated `api::<version>` contract
tree) and its `phoxal-macros` companion, plus a growing set of unpublished
platform runtime binaries (`phoxal-runtime-<name>`) that ship as deployables.
All workspace crates share one version; the library crates are published to
crates.io (see [Releasing](#releasing)).

Design docs are in [`docs/`](docs/): [contract discipline](docs/CONTRACTS.md),
[conventions](docs/CONVENTIONS.md), and [validation](docs/VALIDATION.md).

## Releasing

Merging a `release/vX.Y.Z` PR into `main` tags the release and publishes the
`phoxal` library crate (and its `phoxal-macros` dependency) to crates.io at the
workspace version. See [`.github/workflows/release.yml`](.github/workflows/release.yml).

> **Per-runtime images/binaries — deferred (release-engineering / Phase 6).**
> The greenfield framework rewrite replaced the old crate set and removed
> `Dockerfile.runtime`, the Webots simulator, and the joypad tool, so the
> previous per-runtime GHCR image + multi-platform binary + simulator + joypad
> release matrix is not wired up for the current runtime set yet. Re-introducing
> it (and the `scripts/verify-runtime-release.sh` image-coherence gate, which
> checks images this workflow does not currently produce) is tracked separately.

## License

AGPL-3.0-only — see [LICENSE](LICENSE) for the full license text.
A commercial license is available — see [COMMERCIAL.md](COMMERCIAL.md) and reach out via <https://phoxal.com>.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). DCO sign-off required on every commit.

## Phoxal

- <https://phoxal.com>
- <https://github.com/phoxal>
