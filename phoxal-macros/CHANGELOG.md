# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.44.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.43.2...phoxal-macros-v0.44.0) - 2026-07-30

### Other

- [**breaking**] separate source documents from canonical model ([#358](https://github.com/phoxal/framework/pull/358))

## [0.43.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.42.3...phoxal-macros-v0.43.0) - 2026-07-28

### Other

- [**breaking**] simplify participant authoring ([#350](https://github.com/phoxal/framework/pull/350))

## [0.42.2](https://github.com/phoxal/framework/compare/phoxal-macros-v0.42.1...phoxal-macros-v0.42.2) - 2026-07-27

### Fixed

- *(macros)* keep participant metadata through the linker, default ids to the package name ([#344](https://github.com/phoxal/framework/pull/344))

## [0.42.1](https://github.com/phoxal/framework/compare/phoxal-macros-v0.42.0...phoxal-macros-v0.42.1) - 2026-07-27

### Other

- delete dead code ([#341](https://github.com/phoxal/framework/pull/341))
- *(deps)* raise MSRV to 1.88 and refresh the lockfile ([#339](https://github.com/phoxal/framework/pull/339))

## [0.42.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.41.3...phoxal-macros-v0.42.0) - 2026-07-26

### Other

- [**breaking**] simplify participant contract metadata ([#336](https://github.com/phoxal/framework/pull/336))

## [0.41.2](https://github.com/phoxal/framework/compare/phoxal-macros-v0.41.1...phoxal-macros-v0.41.2) - 2026-07-26

### Other

- simplify topic ownership API ([#328](https://github.com/phoxal/framework/pull/328))

## [0.41.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.40.2...phoxal-macros-v0.41.0) - 2026-07-26

### Added

- [**breaking**] rebuild time, identity, and command liveness

## [0.40.2](https://github.com/phoxal/framework/compare/phoxal-macros-v0.40.1...phoxal-macros-v0.40.2) - 2026-07-25

### Added

- *(simulation)* reset state across Webots epochs ([#315](https://github.com/phoxal/framework/pull/315))

## [0.40.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.39.0...phoxal-macros-v0.40.0) - 2026-07-25

### Added

- *(runtime)* [**breaking**] remove participant systemd notification ([#311](https://github.com/phoxal/framework/pull/311))

## [0.36.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.23.2...phoxal-macros-v0.36.0) - 2026-07-21

### Added

- unify the framework release train ([#289](https://github.com/phoxal/framework/pull/289))

## [0.23.2](https://github.com/phoxal/framework/compare/phoxal-macros-v0.23.1...phoxal-macros-v0.23.2) - 2026-07-20

### Other

- updated the following local packages: phoxal-bus

## [0.23.1](https://github.com/phoxal/framework/compare/phoxal-macros-v0.23.0...phoxal-macros-v0.23.1) - 2026-07-17

### Added

- *(simulator)* remove clock launch controls ([#273](https://github.com/phoxal/framework/pull/273))

## [0.23.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.22.0...phoxal-macros-v0.23.0) - 2026-07-16

### Changed

- [**breaking**] *(runtime)* make tool launches clockless ([#268](https://github.com/phoxal/framework/pull/268))

## [0.22.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.21.3...phoxal-macros-v0.22.0) - 2026-07-15

### Added

- [**breaking**] simplify service topology and restore manual motion ([#264](https://github.com/phoxal/framework/pull/264))

## [0.21.3](https://github.com/phoxal/framework/compare/phoxal-macros-v0.21.2...phoxal-macros-v0.21.3) - 2026-07-14

### Other

- *(api)* adopt stable v1 and preview v2 ([#262](https://github.com/phoxal/framework/pull/262))

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

- *(macros)* embed resolved version-qualified TOPIC in generated metadata (F2-names) ([#198](https://github.com/phoxal/framework/pull/198))
- *(new-model)* add robot()/robot_root() accessors + Querier/Ask contract role

### Other

- *(api)* refine generated contract metadata fields ([#200](https://github.com/phoxal/framework/pull/200))
- phoxal-api refactor — finish nested nodes and version handling ([#195](https://github.com/phoxal/framework/pull/195))
- simplify the participant runner entrypoint (Cleanup)
- *(api)* fold contract identity into the wire key (F-seam)

## [0.20.0](https://github.com/phoxal/framework/compare/phoxal-macros-v0.19.6...phoxal-macros-v0.20.0) - 2026-07-08

### Other

- *(check)* [**breaking**] simplify generated participant metadata (WS5a) ([#189](https://github.com/phoxal/framework/pull/189))

## [0.19.6](https://github.com/phoxal/framework/compare/phoxal-macros-v0.19.5...phoxal-macros-v0.19.6) - 2026-07-06

### Fixed

- *(15)* enforce Tool as a thin raw-bus runner at compile time ([#171](https://github.com/phoxal/framework/pull/171))

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

- *(15,16)* finish authoring taxonomy and contract identity (framework) ([#131](https://github.com/phoxal/framework/pull/131))
- *(15)* Service/Driver authoring kinds replace the Runtime derive ([#128](https://github.com/phoxal/framework/pull/128))

### Other

- *(15)* retire "runtime" from the authoring API (-> participant/behavior) ([#130](https://github.com/phoxal/framework/pull/130))
