# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.7](https://github.com/phoxal/framework/compare/phoxal-component-ddsm115-v0.1.6...phoxal-component-ddsm115-v0.1.7) - 2026-07-11

### Other

- update Cargo.lock dependencies

## [0.1.6](https://github.com/phoxal/framework/releases/tag/phoxal-component-ddsm115-v0.1.6) - 2026-07-10

### Added

- *(model,catalog)* [**breaking**] Phase 7 Band A - five-root-key grammar + package identity ([#180](https://github.com/phoxal/framework/pull/180))
- *(15)* move catalog component drivers into component/* as phoxal-driver-* (#15-c) ([#134](https://github.com/phoxal/framework/pull/134))

### Fixed

- *(11)* webots-rs 0.1.3 - c_char portability unblocks the simulator aarch64 asset ([#163](https://github.com/phoxal/framework/pull/163))
- *(01)* simulator publish flag + discovery guard against cargo-level publish=false ([#162](https://github.com/phoxal/framework/pull/162))
- *(01)* draft-then-publish artifact releases for immutable-releases repos ([#161](https://github.com/phoxal/framework/pull/161))
- *(01)* make git_only artifact releases actually release ([#159](https://github.com/phoxal/framework/pull/159))

### Other

- *(component)* [**breaking**] flatten each component into one crate with binary + assets (F3-flatten) ([#199](https://github.com/phoxal/framework/pull/199))
- delete old authoring model + emit-apis; run_v2 -> run (Cleanup)
- convert remaining 24 participants to new authoring model (P-convert)
- *(api)* fold generation into wire key; drop schema_id/bus_abi/extends (F-seam)
- *(release)* bump changed artifacts ([#193](https://github.com/phoxal/framework/pull/193))
- *(model)* [**breaking**] rename manifest wrappers v1->v0, unify yaml tag to schema:<domain>/v0 (WS5b) ([#191](https://github.com/phoxal/framework/pull/191))
- *(check)* [**breaking**] strip pub/sub/responder/topic/materialization; keep schema_id-by-family + config (WS5a) ([#189](https://github.com/phoxal/framework/pull/189))
- *(release)* bump changed artifacts ([#167](https://github.com/phoxal/framework/pull/167))
- *(release)* artifacts leave release-plz - xtask cuts tags/drafts, release-pr drops from ~100min to minutes ([#165](https://github.com/phoxal/framework/pull/165))
- *(release)* release ([#155](https://github.com/phoxal/framework/pull/155))
- *(01)* activate artifact releases - baseline bump ([#157](https://github.com/phoxal/framework/pull/157))

## [0.1.0](https://github.com/phoxal/framework/releases/tag/phoxal-driver-ddsm115-v0.1.0) - 2026-07-04

### Added

- *(15)* move catalog component drivers into component/* as phoxal-driver-* (#15-c) ([#134](https://github.com/phoxal/framework/pull/134))
