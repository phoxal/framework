# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.61.0](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.60.1...phoxal-runtime-contract-v0.61.0) - 2026-08-14

### Other

- [**breaking**] move protocol and supervisor into the framework train ([#447](https://github.com/phoxal/framework/pull/447))

## [0.59.0](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.58.2...phoxal-runtime-contract-v0.59.0) - 2026-08-11

### Added

- [**breaking**] accept same-line framework trains at every runtime validator ([#438](https://github.com/phoxal/framework/pull/438))

## [0.58.2](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.58.1...phoxal-runtime-contract-v0.58.2) - 2026-08-10

### Added

- make the compatibility promise checkable against published trains ([#437](https://github.com/phoxal/framework/pull/437))

## [0.57.0](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.56.2...phoxal-runtime-contract-v0.57.0) - 2026-08-10

### Added

- [**breaking**] make the framework train version the single compatibility identity ([#429](https://github.com/phoxal/framework/pull/429))

## [0.56.2](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.56.1...phoxal-runtime-contract-v0.56.2) - 2026-08-10

### Added

- *(supervisor)* add execution control contract

## [0.56.0](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.55.0...phoxal-runtime-contract-v0.56.0) - 2026-08-09

### Added

- *(api)* [**breaking**] simplify modular Robot API authoring ([#422](https://github.com/phoxal/framework/pull/422))
- *(bundle)* [**breaking**] separate runtime artifacts from participant instances ([#418](https://github.com/phoxal/framework/pull/418))
- *(launch)* [**breaking**] enforce strict supervised argv ([#413](https://github.com/phoxal/framework/pull/413))
- *(bus)* [**breaking**] enforce owner handles and delivery semantics ([#410](https://github.com/phoxal/framework/pull/410))
- *(api)* [**breaking**] enforce control wire-state invariants
- *(bundle)* [**breaking**] persist validated runtime documents
- *(model)* [**breaking**] remove namespace identity ([#400](https://github.com/phoxal/framework/pull/400))

### Fixed

- *(identity)* [**breaking**] enforce execution and source ownership ([#417](https://github.com/phoxal/framework/pull/417))
- *(runtime)* [**breaking**] enforce static drive topology and motor modes

### Other

- [**breaking**] mechanical code-quality cleanup across the framework ([#398](https://github.com/phoxal/framework/pull/398))

## [0.55.0](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.54.0...phoxal-runtime-contract-v0.55.0) - 2026-08-06

### Added

- [**breaking**] version identities as serde enums, namespaced grammars, and bus attachment APIs ([#395](https://github.com/phoxal/framework/pull/395))

## [0.54.0](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.53.0...phoxal-runtime-contract-v0.54.0) - 2026-08-06

### Added

- [**breaking**] embedded compatibility, session identities, protocol trees, and the finalized bundle ([#393](https://github.com/phoxal/framework/pull/393))

## [0.53.0](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.52.0...phoxal-runtime-contract-v0.53.0) - 2026-08-06

### Added

- [**breaking**] replace the behavior subsystem with the mandatory root brain role ([#391](https://github.com/phoxal/framework/pull/391))

## [0.52.0](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.51.0...phoxal-runtime-contract-v0.52.0) - 2026-08-03

### Added

- [**breaking**] delete the joypad tool and the tool concept ([#386](https://github.com/phoxal/framework/pull/386))

## [0.45.0](https://github.com/phoxal/framework/compare/phoxal-runtime-contract-v0.44.0...phoxal-runtime-contract-v0.45.0) - 2026-07-30

### Other

- [**breaking**] split framework ownership boundaries ([#360](https://github.com/phoxal/framework/pull/360))
