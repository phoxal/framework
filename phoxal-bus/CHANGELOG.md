# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

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

- *(api)* split generation/contract metadata fields + #[phoxal(external)] marker ([#200](https://github.com/phoxal/framework/pull/200))
- delete old authoring model + emit-apis; run_v2 -> run (Cleanup)
- *(api)* fold generation into wire key; drop schema_id/bus_abi/extends (F-seam)

## [0.20.1](https://github.com/phoxal/framework/compare/phoxal-bus-v0.20.0...phoxal-bus-v0.20.1) - 2026-07-02

### Added

- *(07)* gate the raw bus behind the explicit phoxal::raw surface ([#142](https://github.com/phoxal/framework/pull/142))
- *(16)* preview generation lifecycle - preview keyword + api sync-features ([#141](https://github.com/phoxal/framework/pull/141))

### Other

- delete dead docker artifacts; fix stale one-api-version docs ([#138](https://github.com/phoxal/framework/pull/138))

## [0.20.0](https://github.com/phoxal/framework/compare/phoxal-bus-v0.19.1...phoxal-bus-v0.20.0) - 2026-07-01

### Added

- *(15,16)* finish authoring taxonomy + per-contract schema_id (framework) ([#131](https://github.com/phoxal/framework/pull/131))
- *(15)* Service/Driver authoring kinds replace the Runtime derive ([#128](https://github.com/phoxal/framework/pull/128))

### Other

- *(15)* retire "runtime" from the authoring API (-> participant/behavior) ([#130](https://github.com/phoxal/framework/pull/130))
