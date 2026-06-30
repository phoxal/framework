# Phoxal Framework

The Phoxal robot framework as one coherent workspace: the published `phoxal-bus`
ABI crate (the typed bus client + contract/addressing primitives), the published
`phoxal` library crate (engine, model, the dated `api::<version>` contract tree)
and its `phoxal-macros` companion, plus a growing set of unpublished platform
runtime binaries (`phoxal-runtime-<name>`) that ship as deployables.
All workspace crates share one version; the library crates are published to
crates.io (see [Releasing](#releasing)).

Design docs are in [`docs/`](docs/): [contract discipline](docs/CONTRACTS.md),
[conventions](docs/CONVENTIONS.md), and [validation](docs/VALIDATION.md).

## Releasing

Merging a `release/vX.Y.Z` PR into `main` tags the release and then:

- publishes the `phoxal-bus`, `phoxal`, and `phoxal-macros` library crates to
  crates.io at the workspace version;
- builds each official runtime as a multi-arch (`linux/amd64` + `linux/arm64`)
  GHCR image, tagged by **API version**: the immutable `ghcr.io/phoxal/runtime-<name>:<api>-v<version>`
  and the moving `:<api>-stable` channel (e.g. `runtime-drive:y2026_1-stable`).
  These are the pull targets `phoxal-cli` resolves for a robot graph's root
  `api_version` + channel.

See [`.github/workflows/release.yml`](.github/workflows/release.yml) and
[`Dockerfile.runtime`](Dockerfile.runtime). The image tag's api_version is a
lookup convention; `phoxal-cli check` re-proves it by running `emit-apis` on the
resolved image (api-version-availability).

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
