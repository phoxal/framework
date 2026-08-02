# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.46.0](https://github.com/phoxal/framework/compare/phoxal-bus-v0.45.5...phoxal-bus-v0.46.0) - 2026-08-02

### Added

- *(bus)* [**breaking**] open the Zenoh router in-process behind a `router` feature ([#373](https://github.com/phoxal/framework/pull/373))

## [0.45.0](https://github.com/phoxal/framework/compare/phoxal-bus-v0.44.0...phoxal-bus-v0.45.0) - 2026-07-30

### Other

- [**breaking**] split framework ownership boundaries ([#360](https://github.com/phoxal/framework/pull/360))

## [0.43.2](https://github.com/phoxal/framework/compare/phoxal-bus-v0.43.1...phoxal-bus-v0.43.2) - 2026-07-28

### Other

- remove retired CLI check references ([#355](https://github.com/phoxal/framework/pull/355))

## [0.43.0](https://github.com/phoxal/framework/compare/phoxal-bus-v0.42.3...phoxal-bus-v0.43.0) - 2026-07-28

### Other

- [**breaking**] simplify participant authoring ([#350](https://github.com/phoxal/framework/pull/350))

## [0.42.3](https://github.com/phoxal/framework/compare/phoxal-bus-v0.42.2...phoxal-bus-v0.42.3) - 2026-07-27

### Fixed

- *(bus)* retry a failed connect within a bounded window ([#347](https://github.com/phoxal/framework/pull/347))

## [0.42.1](https://github.com/phoxal/framework/compare/phoxal-bus-v0.42.0...phoxal-bus-v0.42.1) - 2026-07-27

### Other

- *(deps)* raise MSRV to 1.88 and refresh the lockfile ([#339](https://github.com/phoxal/framework/pull/339))

## [0.42.0](https://github.com/phoxal/framework/compare/phoxal-bus-v0.41.3...phoxal-bus-v0.42.0) - 2026-07-26

### Other

- [**breaking**] simplify participant contract metadata ([#336](https://github.com/phoxal/framework/pull/336))

## [0.41.3](https://github.com/phoxal/framework/compare/phoxal-bus-v0.41.2...phoxal-bus-v0.41.3) - 2026-07-26

### Other

- cover the runtime behaviour the deleted suites proved ([#333](https://github.com/phoxal/framework/pull/333))

## [0.41.2](https://github.com/phoxal/framework/compare/phoxal-bus-v0.41.1...phoxal-bus-v0.41.2) - 2026-07-26

### Other

- simplify topic ownership API ([#328](https://github.com/phoxal/framework/pull/328))

## [0.41.0](https://github.com/phoxal/framework/compare/phoxal-bus-v0.40.2...phoxal-bus-v0.41.0) - 2026-07-26

### Added

- [**breaking**] rebuild time, identity, and command liveness

## [0.40.2](https://github.com/phoxal/framework/compare/phoxal-bus-v0.40.1...phoxal-bus-v0.40.2) - 2026-07-25

### Added

- *(simulation)* reset state across Webots epochs ([#315](https://github.com/phoxal/framework/pull/315))

## [0.36.2](https://github.com/phoxal/framework/compare/phoxal-bus-v0.36.1...phoxal-bus-v0.36.2) - 2026-07-22

### Added

- qualify participant liveliness by incarnation

## [0.36.0](https://github.com/phoxal/framework/compare/phoxal-bus-v0.22.1...phoxal-bus-v0.36.0) - 2026-07-21

### Added

- unify the framework release train ([#289](https://github.com/phoxal/framework/pull/289))

## [0.22.1](https://github.com/phoxal/framework/compare/phoxal-bus-v0.22.0...phoxal-bus-v0.22.1) - 2026-07-20

### Added

- *(telemetry)* retain runtime diagnostics ([#286](https://github.com/phoxal/framework/pull/286))
- *(tools)* retain queryable log and bus history ([#285](https://github.com/phoxal/framework/pull/285))

### Added

- Expose each bounded `Subscriber`'s cumulative drop-oldest eviction count.
- Add runner-facing interval accounting for exact typed publisher, latest, and
  bounded subscriber buffers.
- Serialize subscriber depth accounting with its ring mutation so a stale
  concurrent update cannot overwrite the current queue depth.

## [0.22.0](https://github.com/phoxal/framework/compare/phoxal-bus-v0.21.3...phoxal-bus-v0.22.0) - 2026-07-20

### Added

- *(bus)* [**breaking**] replace heartbeats with Zenoh liveliness ([#283](https://github.com/phoxal/framework/pull/283))

## [0.21.3](https://github.com/phoxal/framework/compare/phoxal-bus-v0.21.2...phoxal-bus-v0.21.3) - 2026-07-18

### Fixed

- *(tools)* expose router metrics and joypad diagnostics ([#277](https://github.com/phoxal/framework/pull/277))

## [0.21.2](https://github.com/phoxal/framework/compare/phoxal-bus-v0.21.1...phoxal-bus-v0.21.2) - 2026-07-14

### Other

- *(api)* adopt stable v1 and preview v2 ([#262](https://github.com/phoxal/framework/pull/262))

## [0.21.1](https://github.com/phoxal/framework/compare/phoxal-bus-v0.21.0...phoxal-bus-v0.21.1) - 2026-07-12

### Added

- live Webots simulation, docs reconciliation, authoring example ([#229](https://github.com/phoxal/framework/pull/229))

## [0.21.0](https://github.com/phoxal/framework/compare/phoxal-bus-v0.20.1...phoxal-bus-v0.21.0) - 2026-07-10

### Added

- *(runtime)* wire new-model runner (run_v2), coexisting with old (F-runtime)

### Other

- *(api)* refine generated contract metadata fields ([#200](https://github.com/phoxal/framework/pull/200))
- simplify the participant runner entrypoint (Cleanup)
- *(api)* fold contract identity into the wire key (F-seam)

## [0.20.1](https://github.com/phoxal/framework/compare/phoxal-bus-v0.20.0...phoxal-bus-v0.20.1) - 2026-07-02

### Added

- *(07)* gate the raw bus behind the explicit phoxal::raw surface ([#142](https://github.com/phoxal/framework/pull/142))
- *(16)* preview generation lifecycle - preview keyword + api sync-features ([#141](https://github.com/phoxal/framework/pull/141))

### Other

- delete dead docker artifacts; fix stale one-api-version docs ([#138](https://github.com/phoxal/framework/pull/138))

## [0.20.0](https://github.com/phoxal/framework/compare/phoxal-bus-v0.19.1...phoxal-bus-v0.20.0) - 2026-07-01

### Added

- *(15,16)* finish authoring taxonomy and contract identity (framework) ([#131](https://github.com/phoxal/framework/pull/131))
- *(15)* Service/Driver authoring kinds replace the Runtime derive ([#128](https://github.com/phoxal/framework/pull/128))

### Other

- *(15)* retire "runtime" from the authoring API (-> participant/behavior) ([#130](https://github.com/phoxal/framework/pull/130))
