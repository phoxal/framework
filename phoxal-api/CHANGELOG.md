# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.55.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.54.0...phoxal-api-v0.55.0) - 2026-08-06

### Added

- [**breaking**] version identities as serde enums, namespaced grammars, and bus attachment APIs ([#395](https://github.com/phoxal/framework/pull/395))

## [0.54.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.53.0...phoxal-api-v0.54.0) - 2026-08-06

### Added

- [**breaking**] embedded compatibility, session identities, protocol trees, and the finalized bundle ([#393](https://github.com/phoxal/framework/pull/393))

## [0.53.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.52.0...phoxal-api-v0.53.0) - 2026-08-06

### Added

- [**breaking**] replace the behavior subsystem with the mandatory root brain role ([#391](https://github.com/phoxal/framework/pull/391))

## [0.52.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.51.0...phoxal-api-v0.52.0) - 2026-08-03

### Added

- [**breaking**] delete the joypad tool and the tool concept ([#386](https://github.com/phoxal/framework/pull/386))

## [0.51.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.50.0...phoxal-api-v0.51.0) - 2026-08-03

### Added

- *(api)* [**breaking**] dissolve the tool node into supervisor and delete tool/telemetry ([#384](https://github.com/phoxal/framework/pull/384))

## [0.50.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.49.0...phoxal-api-v0.50.0) - 2026-08-02

### Added

- *(api)* [**breaking**] re-home the log contract under supervisor and delete tool/log ([#382](https://github.com/phoxal/framework/pull/382))

## [0.49.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.48.0...phoxal-api-v0.49.0) - 2026-08-02

### Added

- *(api)* [**breaking**] delete the bus and device tools and their contracts ([#380](https://github.com/phoxal/framework/pull/380))

## [0.48.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.47.0...phoxal-api-v0.48.0) - 2026-08-02

### Added

- *(model)* [**breaking**] move the asset resolver down and re-home its contract ([#378](https://github.com/phoxal/framework/pull/378))

## [0.45.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.44.0...phoxal-api-v0.45.0) - 2026-07-30

### Other

- [**breaking**] split framework ownership boundaries ([#360](https://github.com/phoxal/framework/pull/360))

## [0.43.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.42.3...phoxal-api-v0.43.0) - 2026-07-28

### Other

- [**breaking**] simplify participant authoring ([#350](https://github.com/phoxal/framework/pull/350))

## [0.42.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.41.3...phoxal-api-v0.42.0) - 2026-07-26

### Other

- [**breaking**] simplify participant contract metadata ([#336](https://github.com/phoxal/framework/pull/336))

## [0.41.3](https://github.com/phoxal/framework/compare/phoxal-api-v0.41.2...phoxal-api-v0.41.3) - 2026-07-26

### Other

- keep repository coverage to unit contracts ([#331](https://github.com/phoxal/framework/pull/331))

## [0.41.2](https://github.com/phoxal/framework/compare/phoxal-api-v0.41.1...phoxal-api-v0.41.2) - 2026-07-26

### Added

- *(webots)* simulate every component capability but the e-stop ([#326](https://github.com/phoxal/framework/pull/326))

### Other

- simplify topic ownership API ([#328](https://github.com/phoxal/framework/pull/328))

## [0.41.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.40.2...phoxal-api-v0.41.0) - 2026-07-26

### Added

- [**breaking**] rebuild time, identity, and command liveness

## [0.40.2](https://github.com/phoxal/framework/compare/phoxal-api-v0.40.1...phoxal-api-v0.40.2) - 2026-07-25

### Added

- *(simulation)* reset state across Webots epochs ([#315](https://github.com/phoxal/framework/pull/315))

## [0.38.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.37.0...phoxal-api-v0.38.0) - 2026-07-23

### Added

- *(suite)* [**breaking**] add launch profiles and device identity ([#304](https://github.com/phoxal/framework/pull/304))

## [0.36.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.22.1...phoxal-api-v0.36.0) - 2026-07-21

### Added

- unify the framework release train ([#289](https://github.com/phoxal/framework/pull/289))

## [0.22.1](https://github.com/phoxal/framework/compare/phoxal-api-v0.22.0...phoxal-api-v0.22.1) - 2026-07-20

### Added

- *(device)* add per-root resource telemetry ([#287](https://github.com/phoxal/framework/pull/287))
- *(telemetry)* retain runtime diagnostics ([#286](https://github.com/phoxal/framework/pull/286))
- *(tools)* retain queryable log and bus history ([#285](https://github.com/phoxal/framework/pull/285))

### Added

- Add stable bounded snapshot/follow contracts for per-robot structured logs
  and bus-rate history under `v1::tool`.
- Disclose cumulative tool-log ingest-ring loss on snapshots and follow items.
- Add stable runtime-performance rollup, bounded history query, and follow
  contracts under `v1::tool::runtime`.
- Add capability-aware whole-device samples and bounded history contracts under
  `v1::tool::device`.
- Disclose runtime-retention identity truncation and aggregate excess or
  oversized topic rows instead of retaining unbounded wire shapes.

### Removed

- Remove the superseded preview `v2::router::Metrics` live-only contract.
- Remove the preview per-process CPU/RSS telemetry contract.
- Remove the superseded preview host telemetry contract.

## [0.22.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.21.3...phoxal-api-v0.22.0) - 2026-07-20

### Added

- *(bus)* [**breaking**] replace heartbeats with Zenoh liveliness ([#283](https://github.com/phoxal/framework/pull/283))

## [0.21.3](https://github.com/phoxal/framework/compare/phoxal-api-v0.21.2...phoxal-api-v0.21.3) - 2026-07-19

### Added

- extract infrastructure router ([#279](https://github.com/phoxal/framework/pull/279))

## [0.21.2](https://github.com/phoxal/framework/compare/phoxal-api-v0.21.1...phoxal-api-v0.21.2) - 2026-07-18

### Fixed

- *(tools)* expose router metrics and joypad diagnostics ([#277](https://github.com/phoxal/framework/pull/277))

## [0.21.1](https://github.com/phoxal/framework/compare/phoxal-api-v0.21.0...phoxal-api-v0.21.1) - 2026-07-17

### Added

- *(tools)* expose session telemetry and input authority ([#275](https://github.com/phoxal/framework/pull/275))

## [0.21.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.20.5...phoxal-api-v0.21.0) - 2026-07-15

### Added

- [**breaking**] simplify service topology and restore manual motion ([#264](https://github.com/phoxal/framework/pull/264))

## [0.20.5](https://github.com/phoxal/framework/compare/phoxal-api-v0.20.4...phoxal-api-v0.20.5) - 2026-07-14

### Other

- *(api)* adopt stable v1 and preview v2 ([#262](https://github.com/phoxal/framework/pull/262))

## [0.20.4](https://github.com/phoxal/framework/compare/phoxal-api-v0.20.3...phoxal-api-v0.20.4) - 2026-07-13

### Added

- *(simulation)* update the advancing clock contract ([#256](https://github.com/phoxal/framework/pull/256))

## [0.20.3](https://github.com/phoxal/framework/compare/phoxal-api-v0.20.2...phoxal-api-v0.20.3) - 2026-07-13

### Added

- *(cli-ux)* preview v2 contracts + telemetry/joypad/router tools (framework half) ([#252](https://github.com/phoxal/framework/pull/252))

## [0.20.2](https://github.com/phoxal/framework/compare/phoxal-api-v0.20.1...phoxal-api-v0.20.2) - 2026-07-12

### Added

- *(simulation)* supervisor pulls robot spawn set over the bus, not --config ([#235](https://github.com/phoxal/framework/pull/235))
- live Webots simulation, docs reconciliation, authoring example ([#229](https://github.com/phoxal/framework/pull/229))

## [0.20.1](https://github.com/phoxal/framework/compare/phoxal-api-v0.20.0...phoxal-api-v0.20.1) - 2026-07-11

### Other

- updated the following local packages: phoxal-macros

## [0.20.0](https://github.com/phoxal/framework/compare/phoxal-api-v0.19.5...phoxal-api-v0.20.0) - 2026-07-10

### Other

- phoxal-api refactor — finish batch (nested nodes, Declares/server gating, mixed API versions) ([#195](https://github.com/phoxal/framework/pull/195))
- *(api)* fold contract identity into the wire key (F-seam)

## [0.19.5](https://github.com/phoxal/framework/compare/phoxal-api-v0.19.4...phoxal-api-v0.19.5) - 2026-07-08

### Other

- updated the following local packages: phoxal-macros

## [0.19.4](https://github.com/phoxal/framework/compare/phoxal-api-v0.19.3...phoxal-api-v0.19.4) - 2026-07-03

### Added

- *(04)* bus log layer + tool-router and tool-joypad artifacts ([#147](https://github.com/phoxal/framework/pull/147))

## [0.19.3](https://github.com/phoxal/framework/compare/phoxal-api-v0.19.2...phoxal-api-v0.19.3) - 2026-07-02

### Added

- *(16)* D5 manifest model + generation lifecycle gates (schema-diff, preview-impact, promotion + freeze) ([#144](https://github.com/phoxal/framework/pull/144))
- *(16)* preview generation lifecycle - preview keyword + api sync-features ([#141](https://github.com/phoxal/framework/pull/141))

### Other

- delete dead docker artifacts; fix stale one-api-version docs ([#138](https://github.com/phoxal/framework/pull/138))

## [0.19.2](https://github.com/phoxal/framework/compare/phoxal-api-v0.19.1...phoxal-api-v0.19.2) - 2026-07-01

### Added

- *(15,16)* finish authoring taxonomy and contract identity (framework) ([#131](https://github.com/phoxal/framework/pull/131))
- *(15)* Service/Driver authoring kinds replace the Runtime derive ([#128](https://github.com/phoxal/framework/pull/128))

### Other

- *(15)* retire "runtime" from the authoring API (-> participant/behavior) ([#130](https://github.com/phoxal/framework/pull/130))
