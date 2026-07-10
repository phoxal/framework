# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.19.7](https://github.com/phoxal/framework/releases/tag/phoxal-service-follow-v0.19.7) - 2026-07-10

### Added

- *(15,16)* finish authoring taxonomy + per-contract schema_id (framework) ([#131](https://github.com/phoxal/framework/pull/131))

### Fixed

- *(11)* webots-rs 0.1.3 - c_char portability unblocks the simulator aarch64 asset ([#163](https://github.com/phoxal/framework/pull/163))
- *(01)* simulator publish flag + discovery guard against cargo-level publish=false ([#162](https://github.com/phoxal/framework/pull/162))
- *(01)* draft-then-publish artifact releases for immutable-releases repos ([#161](https://github.com/phoxal/framework/pull/161))
- *(01)* make git_only artifact releases actually release ([#159](https://github.com/phoxal/framework/pull/159))

### Other

- delete old authoring model + emit-apis; run_v2 -> run (Cleanup)
- convert remaining 24 participants to new authoring model (P-convert)
- *(api)* fold generation into wire key; drop schema_id/bus_abi/extends (F-seam)
- *(release)* bump changed artifacts ([#193](https://github.com/phoxal/framework/pull/193))
- *(check)* [**breaking**] strip pub/sub/responder/topic/materialization; keep schema_id-by-family + config (WS5a) ([#189](https://github.com/phoxal/framework/pull/189))
- *(release)* bump changed artifacts ([#167](https://github.com/phoxal/framework/pull/167))
- *(release)* artifacts leave release-plz - xtask cuts tags/drafts, release-pr drops from ~100min to minutes ([#165](https://github.com/phoxal/framework/pull/165))
- *(release)* release ([#155](https://github.com/phoxal/framework/pull/155))
- *(01)* activate artifact releases - baseline bump ([#157](https://github.com/phoxal/framework/pull/157))

## [0.19.1](https://github.com/phoxal/framework/releases/tag/phoxal-service-follow-v0.19.1) - 2026-07-04

### Added

- *(15,16)* finish authoring taxonomy + per-contract schema_id (framework) ([#131](https://github.com/phoxal/framework/pull/131))
