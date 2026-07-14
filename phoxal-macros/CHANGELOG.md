# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.21.2](https://github.com/phoxal/framework/compare/phoxal-macros-v0.21.1...phoxal-macros-v0.21.2) - 2026-07-13

### Added

- *(cli-ux)* preview v2 contracts + telemetry/joypad/router tools (framework half) ([#252](https://github.com/phoxal/framework/pull/252))

## [0.21.1](https://github.com/phoxal/framework/compare/phoxal-macros-v0.21.0...phoxal-macros-v0.21.1) - 2026-07-12

### Added

- live Webots simulation, docs reconciliation, authoring example ([#229](https://github.com/phoxal/framework/pull/229))

## [0.21.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.20.1...phoxal-macros-v0.21.0) - 2026-07-11

### Added

- *(config)* [**breaking**] const-schema Config derive + schema slot in metadata section ([#214](https://github.com/phoxal/framework/pull/214))

## [0.20.1](https://github.com/phoxal/framework/compare/phoxal-macros-v0.20.0...phoxal-macros-v0.20.1) - 2026-07-10

### Added

- *(runtime)* wire new-model runner (run_v2), coexisting with old (F-runtime)
- *(macros)* add new participant authoring model alongside old (F-macros)

### Fixed

- *(macros)* embed resolved version-qualified TOPIC in the Api metadata section (F2-names) ([#198](https://github.com/phoxal/framework/pull/198))
- *(new-model)* add robot()/robot_root() accessors + Querier/Ask contract role

### Other

- *(api)* split generation/contract metadata fields + #[phoxal(external)] marker ([#200](https://github.com/phoxal/framework/pull/200))
- phoxal-api refactor — finish batch (nested nodes, Declares/server gating, mixed API versions) ([#195](https://github.com/phoxal/framework/pull/195))
- delete old authoring model + emit-apis; run_v2 -> run (Cleanup)
- *(api)* fold generation into wire key; drop schema_id/bus_abi/extends (F-seam)

## [0.20.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.19.6...phoxal-macros-v0.20.0) - 2026-07-08

### Other

- *(check)* [**breaking**] strip pub/sub/responder/topic/materialization; keep schema_id-by-family + config (WS5a) ([#189](https://github.com/phoxal/framework/pull/189))

## [0.19.6](https://github.com/phoxal/framework/compare/phoxal-macros-v0.19.5...phoxal-macros-v0.19.6) - 2026-07-06

### Fixed

- *(15)* enforce Tool as a thin runner - reject #[step]/#[server] at compile time ([#171](https://github.com/phoxal/framework/pull/171))

## [0.19.5](https://github.com/phoxal/framework/compare/phoxal-macros-v0.19.4...phoxal-macros-v0.19.5) - 2026-07-03

### Added

- *(04)* bus log layer + tool-router and tool-joypad artifacts ([#147](https://github.com/phoxal/framework/pull/147))

## [0.19.4](https://github.com/phoxal/framework/compare/phoxal-macros-v0.19.3...phoxal-macros-v0.19.4) - 2026-07-02

### Added

- *(16)* D5 manifest model + generation lifecycle gates (schema-diff, preview-impact, promotion + freeze) ([#144](https://github.com/phoxal/framework/pull/144))
- *(07)* gate the raw bus behind the explicit phoxal::raw surface ([#142](https://github.com/phoxal/framework/pull/142))
- *(16)* preview generation lifecycle - preview keyword + api sync-features ([#141](https://github.com/phoxal/framework/pull/141))

### Other

- delete dead docker artifacts; fix stale one-api-version docs ([#138](https://github.com/phoxal/framework/pull/138))

## [0.19.3](https://github.com/phoxal/framework/compare/phoxal-macros-v0.19.2...phoxal-macros-v0.19.3) - 2026-07-01

### Added

- *(02,05)* participant_class + shared phoxal::check graph core (framework) ([#132](https://github.com/phoxal/framework/pull/132))

## [0.19.2](https://github.com/phoxal/framework/compare/phoxal-macros-v0.19.1...phoxal-macros-v0.19.2) - 2026-07-01

### Added

- *(15,16)* finish authoring taxonomy + per-contract schema_id (framework) ([#131](https://github.com/phoxal/framework/pull/131))
- *(15)* Service/Driver authoring kinds replace the Runtime derive ([#128](https://github.com/phoxal/framework/pull/128))

### Other

- *(15)* retire "runtime" from the authoring API (-> participant/behavior) ([#130](https://github.com/phoxal/framework/pull/130))
