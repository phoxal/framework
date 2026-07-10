# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.1.2](https://github.com/phoxal/framework/releases/tag/phoxal-simulator-webots-supervisor-v0.1.2) - 2026-07-10

### Added

- *(simulator)* supervisor spawns robot nodes from descriptors at runtime ([#175](https://github.com/phoxal/framework/pull/175))
- *(P6-1)* split the Webots simulator monolith into two artifacts ([#174](https://github.com/phoxal/framework/pull/174))

### Other

- delete old authoring model + emit-apis; run_v2 -> run (Cleanup)
- convert remaining 24 participants to new authoring model (P-convert)
- *(api)* fold generation into wire key; drop schema_id/bus_abi/extends (F-seam)
- *(release)* bump changed artifacts ([#193](https://github.com/phoxal/framework/pull/193))
- *(check)* [**breaking**] strip pub/sub/responder/topic/materialization; keep schema_id-by-family + config (WS5a) ([#189](https://github.com/phoxal/framework/pull/189))
- *(release)* bump changed artifacts ([#168](https://github.com/phoxal/framework/pull/168))
