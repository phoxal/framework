# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.25.0](https://github.com/phoxal/framework/compare/phoxal-v0.24.1...phoxal-v0.25.0) - 2026-07-04

### Added

- *(19)* artifacts pins model - the unified pin map with path overrides ([#152](https://github.com/phoxal/framework/pull/152))

## [0.24.1](https://github.com/phoxal/framework/compare/phoxal-v0.24.0...phoxal-v0.24.1) - 2026-07-04

### Added

- *(03,04)* runner heartbeat + dedicated-thread watchdog ([#150](https://github.com/phoxal/framework/pull/150))

## [0.24.0](https://github.com/phoxal/framework/compare/phoxal-v0.23.0...phoxal-v0.24.0) - 2026-07-03

### Added

- *(10)* contract substitution in the shared check core ([#148](https://github.com/phoxal/framework/pull/148))

## [0.23.0](https://github.com/phoxal/framework/compare/phoxal-v0.22.0...phoxal-v0.23.0) - 2026-07-03

### Added

- *(04)* bus log layer + tool-router and tool-joypad artifacts ([#147](https://github.com/phoxal/framework/pull/147))
- *(18)* the clap launch contract + robot_root rename ([#145](https://github.com/phoxal/framework/pull/145))

## [0.22.0](https://github.com/phoxal/framework/compare/phoxal-v0.21.0...phoxal-v0.22.0) - 2026-07-02

### Added

- *(16)* D5 manifest model + generation lifecycle gates (schema-diff, preview-impact, promotion + freeze) ([#144](https://github.com/phoxal/framework/pull/144))
- *(07)* gate the raw bus behind the explicit phoxal::raw surface ([#142](https://github.com/phoxal/framework/pull/142))
- *(03)* runner emits sd_notify(READY=1) when NOTIFY_SOCKET is set ([#140](https://github.com/phoxal/framework/pull/140))
- *(15)* move catalog component drivers into component/* as phoxal-driver-* (#15-c) ([#134](https://github.com/phoxal/framework/pull/134))

### Other

- delete dead docker artifacts; fix stale one-api-version docs ([#138](https://github.com/phoxal/framework/pull/138))

## [0.21.0](https://github.com/phoxal/framework/compare/phoxal-v0.20.0...phoxal-v0.21.0) - 2026-07-01

### Added

- *(02,05)* participant_class + shared phoxal::check graph core (framework) ([#132](https://github.com/phoxal/framework/pull/132))

## [0.20.0](https://github.com/phoxal/framework/compare/phoxal-v0.19.1...phoxal-v0.20.0) - 2026-07-01

### Added

- *(15,16)* finish authoring taxonomy + per-contract schema_id (framework) ([#131](https://github.com/phoxal/framework/pull/131))
- *(15)* Service/Driver authoring kinds replace the Runtime derive ([#128](https://github.com/phoxal/framework/pull/128))

### Other

- *(15)* retire "runtime" from the authoring API (-> participant/behavior) ([#130](https://github.com/phoxal/framework/pull/130))
