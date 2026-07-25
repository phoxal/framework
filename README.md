# Phoxal Framework

The Phoxal robot framework as one coherent, versioned train. `phoxal-bus`,
`phoxal-macros`, `phoxal-api`, the `phoxal` facade, and every official service,
component, tool, simulator, and infrastructure artifact inherit one workspace
SemVer. The train exports its selected complete API as `phoxal::api`; concrete
API revisions remain available from `phoxal-api` for adapters.
Official per-robot tools ship the same way. `phoxal-tool-log` retains the newest
1,000 structured `v0.1::logs` events, while `phoxal-tool-bus` retains the newest
60 completed one-second traffic windows plus current counters. Both expose a bounded snapshot query and a live
follow topic under `v0.1::tool`; consumers re-query after an opaque process
generation change or a sequence gap.
Every participant runner also publishes at most one portable
`v0.1::tool::runtime::Rollup` per host-monotonic grid interval. It reports
scheduled-step timing and bounded typed-bus buffer pressure without OS process
sampling or participant-authored instrumentation. `phoxal-tool-telemetry`
clamps retained participant/topic identities and topic rows, retains the newest
five minutes subject to both record and byte caps, and exposes the same
snapshot/cursor/follow recovery model under `v0.1::tool::runtime`.
One runner-owned `phoxal-tool-device` per robot root publishes truthful,
capability-aware whole-device observations under `v0.1::tool::device`.
Every participant in a supervised run carries that run's `ExecutionId`, which
scopes the bus root itself, so samplers in one project are already grouped
without a second identity. Unsupported measurements are absent rather than
fabricated as zero.
Per-root `phoxal-tool-telemetry` retains those samples for five minutes with
record and byte bounds and exposes the same cursor/snapshot/follow recovery
model. Device totals remain separate from participant runtime measurements.
The four library crates are published to crates.io and the exact official
artifacts are published with an immutable per-train `suite.json` on GitHub.

Design docs are in [`docs/`](docs/): [contract discipline](docs/CONTRACTS.md),
[conventions](docs/CONVENTIONS.md), and [validation](docs/VALIDATION.md).

## Getting started

Robot projects are hand-authored: a `robot.yaml` manifest, a `structure.urdf`,
component definitions, and user service and tool crates declared in the
manifest's `services:` and `tools:` maps.
[`docs/GETTING_STARTED.md`](docs/GETTING_STARTED.md) walks through authoring
one using the checked-in [`examples/hello-rover`](examples/hello-rover)
project, and links the editor-facing
[`examples/robot.schema.json`](examples/robot.schema.json) JSON Schema for
`robot.yaml`.

## Releasing

The root `[workspace.package].version` is the only train version. Every member
inherits it, and release-plz prepares one grouped
`chore(release): release v<version>` PR.
`cargo xtask release verify` checks the complete official source set and API
graph; `cargo xtask release suite` then verifies every staged target archive
and produces the immutable, artifact-only `phoxal.suite/v0` `suite.json`.

Publication is resumable and GitHub-first. CI creates or resumes a non-latest
draft `v<version>` release, uploads the immutable artifacts and descriptor,
publishes `phoxal-bus`, `phoxal-macros`, `phoxal-api`, and finally the public
`phoxal` facade, waits for those exact versions to be observable on crates.io,
then completes and marks the GitHub train latest. A retry reuses the same tag
and replaces assets only while the release remains a draft.

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
