# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.34.2](https://github.com/phoxal/framework/compare/phoxal-v0.34.1...phoxal-v0.34.2) - 2026-07-18

### Fixed

- *(tools)* expose router metrics and joypad diagnostics ([#277](https://github.com/phoxal/framework/pull/277))

## [0.34.1](https://github.com/phoxal/framework/compare/phoxal-v0.34.0...phoxal-v0.34.1) - 2026-07-17

### Added

- *(simulator)* remove clock launch controls ([#273](https://github.com/phoxal/framework/pull/273))

## [0.34.0](https://github.com/phoxal/framework/compare/phoxal-v0.33.0...phoxal-v0.34.0) - 2026-07-16

### Changed

- [**breaking**] *(runtime)* make tool launches clockless ([#268](https://github.com/phoxal/framework/pull/268))

## [0.33.0](https://github.com/phoxal/framework/compare/phoxal-v0.32.6...phoxal-v0.33.0) - 2026-07-15

### Added

- [**breaking**] simplify service topology and restore manual motion ([#264](https://github.com/phoxal/framework/pull/264))

## [0.32.6](https://github.com/phoxal/framework/compare/phoxal-v0.32.5...phoxal-v0.32.6) - 2026-07-14

### Other

- *(api)* adopt stable v1 and preview v2 ([#262](https://github.com/phoxal/framework/pull/262))

## [0.32.5](https://github.com/phoxal/framework/compare/phoxal-v0.32.4...phoxal-v0.32.5) - 2026-07-13

### Added

- *(simulation)* update the advancing clock contract ([#256](https://github.com/phoxal/framework/pull/256))

## [0.32.4](https://github.com/phoxal/framework/compare/phoxal-v0.32.3...phoxal-v0.32.4) - 2026-07-13

### Added

- *(cli-ux)* preview v2 contracts + telemetry/joypad/router tools (framework half) ([#252](https://github.com/phoxal/framework/pull/252))

### Other

- *(release)* rename asset scope ([#250](https://github.com/phoxal/framework/pull/250))

## [0.32.3](https://github.com/phoxal/framework/compare/phoxal-v0.32.2...phoxal-v0.32.3) - 2026-07-12

### Added

- *(release)* publish only changed artifacts ([#239](https://github.com/phoxal/framework/pull/239))

## [0.32.2](https://github.com/phoxal/framework/compare/phoxal-v0.32.1...phoxal-v0.32.2) - 2026-07-12

### Added

- live Webots simulation, docs reconciliation, authoring example ([#229](https://github.com/phoxal/framework/pull/229))

## [0.32.1](https://github.com/phoxal/framework/compare/phoxal-v0.32.0...phoxal-v0.32.1) - 2026-07-11

### Added

- *(release)* git-diff per-artifact versioning (change one binary → only it bumps) ([#227](https://github.com/phoxal/framework/pull/227))

## [0.32.0](https://github.com/phoxal/framework/compare/phoxal-v0.31.1...phoxal-v0.32.0) - 2026-07-11

### Added

- *(config)* [**breaking**] const-schema Config derive + schema slot in metadata section ([#214](https://github.com/phoxal/framework/pull/214))

## [0.31.1](https://github.com/phoxal/framework/compare/phoxal-v0.31.0...phoxal-v0.31.1) - 2026-07-10

### Fixed

- *(participant)* de-flake simulation clock-feed scheduler test ([#209](https://github.com/phoxal/framework/pull/209))

## [0.31.0](https://github.com/phoxal/framework/compare/phoxal-v0.30.0...phoxal-v0.31.0) - 2026-07-10

### Added

- *(check)* coherence pass - per-participant pub/sub overlap + every-ask-matched (coherence-B) ([#203](https://github.com/phoxal/framework/pull/203))
- *(xtask)* read metadata from binary section; drop schema_id/freeze tooling; frozen-gen release check (X-tools)
- *(runtime)* wire new-model runner (run_v2), coexisting with old (F-runtime)
- *(macros)* add new participant authoring model alongside old (F-macros)

### Fixed

- *(macros)* embed resolved version-qualified TOPIC in the Api metadata section (F2-names) ([#198](https://github.com/phoxal/framework/pull/198))
- *(new-model)* blanket ParticipantConfig for Option<T>
- *(new-model)* add robot()/robot_root() accessors + Querier/Ask contract role

### Other

- *(api)* split generation/contract metadata fields + #[phoxal(external)] marker ([#200](https://github.com/phoxal/framework/pull/200))
- *(component)* [**breaking**] flatten each component into one crate with binary + assets (F3-flatten) ([#199](https://github.com/phoxal/framework/pull/199))
- *(ci)* build-snapshot release model (phoxal.catalog/v0) ([#196](https://github.com/phoxal/framework/pull/196))
- phoxal-api refactor — finish batch (nested nodes, Declares/server gating, mixed API versions) ([#195](https://github.com/phoxal/framework/pull/195))
- delete old authoring model + emit-apis; run_v2 -> run (Cleanup)
- *(api)* fold generation into wire key; drop schema_id/bus_abi/extends (F-seam)

## [0.30.0](https://github.com/phoxal/framework/compare/phoxal-v0.29.0...phoxal-v0.30.0) - 2026-07-08

### Other

- *(model)* [**breaking**] rename manifest wrappers v1->v0, unify yaml tag to schema:<domain>/v0 (WS5b) ([#191](https://github.com/phoxal/framework/pull/191))
- *(check)* [**breaking**] strip pub/sub/responder/topic/materialization; keep schema_id-by-family + config (WS5a) ([#189](https://github.com/phoxal/framework/pull/189))

## [0.29.0](https://github.com/phoxal/framework/compare/phoxal-v0.28.0...phoxal-v0.29.0) - 2026-07-07

### Other

- *(catalog)* [**breaking**] lean phoxal-artifacts.json, single phoxal::catalog schema, inline emit-apis (WS1) ([#187](https://github.com/phoxal/framework/pull/187))

## [0.28.0](https://github.com/phoxal/framework/compare/phoxal-v0.27.0...phoxal-v0.28.0) - 2026-07-07

### Other

- *(check)* [**breaking**] remove the sim substitution-completeness check (D16 simplified) ([#185](https://github.com/phoxal/framework/pull/185))

## [0.27.0](https://github.com/phoxal/framework/compare/phoxal-v0.26.1...phoxal-v0.27.0) - 2026-07-06

### Added

- *(model,catalog)* [**breaking**] Phase 7 Band A - five-root-key grammar + package identity ([#180](https://github.com/phoxal/framework/pull/180))

## [0.26.1](https://github.com/phoxal/framework/compare/phoxal-v0.26.0...phoxal-v0.26.1) - 2026-07-06

### Added

- *(runner)* drive the simulation scheduler from the live simulation/clock feed ([#176](https://github.com/phoxal/framework/pull/176))
- *(09)* clock-driven step scheduling via StepScheduler ([#173](https://github.com/phoxal/framework/pull/173))
- *(08)* SetupContext::spawn_managed - runner-owned background tasks ([#170](https://github.com/phoxal/framework/pull/170))

### Fixed

- *(15)* enforce Tool as a thin runner - reject #[step]/#[server] at compile time ([#171](https://github.com/phoxal/framework/pull/171))

## [0.26.0](https://github.com/phoxal/framework/compare/phoxal-v0.25.0...phoxal-v0.26.0) - 2026-07-04

### Fixed

- *(model)* add emergency_stop to robot Parameters capability kinds ([#164](https://github.com/phoxal/framework/pull/164))

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
