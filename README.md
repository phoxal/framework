# Phoxal Framework

The Phoxal robot framework as one coherent workspace: one published `phoxal`
library crate (`bus`, `util`, `model::{component,robot,structure,simulation}`,
`spatial`, `api::<name>`, `runtime`, and feature-gated `scenario`), the
complete set of 17 unpublished platform runtime binaries
(`phoxal-runtime-<name>`), and `orb-slam3-sys`. Released together, the crate and
every runtime image share the same workspace version.

Design docs are in [`docs/`](docs/): [contract discipline](docs/CONTRACTS.md),
[conventions](docs/CONVENTIONS.md), and [validation](docs/VALIDATION.md).

## Releasing

A release builds and pushes a GHCR image for every runtime binary
(`ghcr.io/phoxal/runtime-<name>`), each a multi-arch (`linux/amd64` +
`linux/arm64`) manifest tagged at the workspace version, plus per-target
runtime binaries. See [`.github/workflows/release.yml`](.github/workflows/release.yml).

After a release, prove the image set is coherent — every runtime binary has a
published, pullable multi-arch image:

```sh
scripts/verify-runtime-release.sh [VERSION]   # defaults to the Cargo.toml version
```

The runtime set is derived from the workspace (`runtime/<name>/Cargo.toml`),
so the gate fails if a runtime binary is added without a matching published
image — keeping the release matrix, the runtime binaries, and the `phoxal-cli`
platform-runtime catalog in sync. It uses `docker buildx imagetools inspect`,
which queries the registry directly (no Docker daemon needed); `docker login
ghcr.io` first for private packages.

## License

AGPL-3.0-only — see [LICENSE](LICENSE) for the full license text.
A commercial license is available — see [COMMERCIAL.md](COMMERCIAL.md) and reach out via <https://phoxal.com>.

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md). DCO sign-off required on every commit.

## Phoxal

- <https://phoxal.com>
- <https://github.com/phoxal>
