# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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
- *(api)* fold generation into wire key; drop schema_id/bus_abi/extends (F-seam)

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

- *(15,16)* finish authoring taxonomy + per-contract schema_id (framework) ([#131](https://github.com/phoxal/framework/pull/131))
- *(15)* Service/Driver authoring kinds replace the Runtime derive ([#128](https://github.com/phoxal/framework/pull/128))

### Other

- *(15)* retire "runtime" from the authoring API (-> participant/behavior) ([#130](https://github.com/phoxal/framework/pull/130))
