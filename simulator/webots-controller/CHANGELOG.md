# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.2.0](https://github.com/phoxal/framework/compare/phoxal-simulator-webots-controller-v0.1.2...phoxal-simulator-webots-controller-v0.2.0) - 2026-07-11

### Added

- *(config)* [**breaking**] const-schema Config derive + schema slot in metadata section ([#214](https://github.com/phoxal/framework/pull/214))

## [0.1.2](https://github.com/phoxal/framework/releases/tag/phoxal-simulator-webots-controller-v0.1.2) - 2026-07-10

### Added

- *(model,catalog)* [**breaking**] Phase 7 Band A - five-root-key grammar + package identity ([#180](https://github.com/phoxal/framework/pull/180))
- *(P6-1)* split the Webots simulator monolith into two artifacts ([#174](https://github.com/phoxal/framework/pull/174))

### Other

- *(component)* [**breaking**] flatten each component into one crate with binary + assets (F3-flatten) ([#199](https://github.com/phoxal/framework/pull/199))
- delete old authoring model + emit-apis; run_v2 -> run (Cleanup)
- convert remaining 24 participants to new authoring model (P-convert)
- *(api)* fold generation into wire key; drop schema_id/bus_abi/extends (F-seam)
- *(release)* bump changed artifacts ([#193](https://github.com/phoxal/framework/pull/193))
- *(model)* [**breaking**] rename manifest wrappers v1->v0, unify yaml tag to schema:<domain>/v0 (WS5b) ([#191](https://github.com/phoxal/framework/pull/191))
- *(check)* [**breaking**] strip pub/sub/responder/topic/materialization; keep schema_id-by-family + config (WS5a) ([#189](https://github.com/phoxal/framework/pull/189))
- *(release)* bump changed artifacts ([#168](https://github.com/phoxal/framework/pull/168))
