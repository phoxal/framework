# Phoxal Framework

The Phoxal robot framework as one coherent workspace: the `phoxal` library crate
plus the complete mandatory platform runtime set. Released together, every
runtime image ships at the same workspace version.

Design docs are in [`docs/`](docs/): [contract discipline](docs/CONTRACTS.md),
[conventions](docs/CONVENTIONS.md), and [validation](docs/VALIDATION.md).

## Releasing

A release builds and pushes a GHCR image for every runtime crate
(`ghcr.io/phoxal/runtime-<name>`), each a multi-arch (`linux/amd64` +
`linux/arm64`) manifest tagged at the workspace version, plus per-target
runtime binaries. See [`.github/workflows/release.yml`](.github/workflows/release.yml).

After a release, prove the image set is coherent — every runtime crate has a
published, pullable multi-arch image:

```sh
scripts/verify-runtime-release.sh [VERSION]   # defaults to the Cargo.toml version
```

The runtime set is derived from the workspace (`runtime/<name>/Cargo.toml`),
so the gate fails if a runtime crate is added without a matching published
image — keeping the release matrix, the runtime crates, and the `phoxal-cli`
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
