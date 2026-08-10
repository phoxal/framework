# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.58.0](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.57.0...phoxal-manifest-v0.58.0) - 2026-08-10

### Other

- [**breaking**] replace the hardened bundle filesystem layer with plain std::fs ([#432](https://github.com/phoxal/framework/pull/432))

## [0.57.0](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.56.2...phoxal-manifest-v0.57.0) - 2026-08-10

### Other

- reorganize the workspace directory layout ([#427](https://github.com/phoxal/framework/pull/427))

## [0.56.2](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.56.1...phoxal-manifest-v0.56.2) - 2026-08-10

### Added

- *(supervisor)* add execution control contract

## [0.56.1](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.56.0...phoxal-manifest-v0.56.1) - 2026-08-10

### Other

- enforce final framework hygiene ([#423](https://github.com/phoxal/framework/pull/423))

## [0.56.0](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.55.0...phoxal-manifest-v0.56.0) - 2026-08-09

### Added

- *(model)* [**breaking**] preserve authored safety runtime truth ([#420](https://github.com/phoxal/framework/pull/420))
- *(bundle)* [**breaking**] separate runtime artifacts from participant instances ([#418](https://github.com/phoxal/framework/pull/418))
- [**breaking**] enforce revisioned map safety and robot footprints ([#411](https://github.com/phoxal/framework/pull/411))
- *(bundle)* [**breaking**] persist validated runtime documents
- *(model)* [**breaking**] remove namespace identity ([#400](https://github.com/phoxal/framework/pull/400))

### Other

- enforce the panic and stub rules with clippy ([#399](https://github.com/phoxal/framework/pull/399))
- [**breaking**] mechanical code-quality cleanup across the framework ([#398](https://github.com/phoxal/framework/pull/398))

## [0.55.0](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.54.0...phoxal-manifest-v0.55.0) - 2026-08-06

### Added

- [**breaking**] version identities as serde enums, namespaced grammars, and bus attachment APIs ([#395](https://github.com/phoxal/framework/pull/395))

## [0.54.0](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.53.0...phoxal-manifest-v0.54.0) - 2026-08-06

### Added

- [**breaking**] embedded compatibility, session identities, protocol trees, and the finalized bundle ([#393](https://github.com/phoxal/framework/pull/393))

## [0.53.0](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.52.0...phoxal-manifest-v0.53.0) - 2026-08-06

### Added

- [**breaking**] replace the behavior subsystem with the mandatory root brain role ([#391](https://github.com/phoxal/framework/pull/391))

### Other

- drop the public API snapshot and isolated packaging jobs ([#390](https://github.com/phoxal/framework/pull/390))
- *(manifest)* [**breaking**] make the schema tag select a versioned manifest enum ([#388](https://github.com/phoxal/framework/pull/388))

## [0.52.0](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.51.0...phoxal-manifest-v0.52.0) - 2026-08-03

### Added

- [**breaking**] delete the joypad tool and the tool concept ([#386](https://github.com/phoxal/framework/pull/386))

## [0.45.5](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.45.4...phoxal-manifest-v0.45.5) - 2026-08-01

### Added

- *(manifest)* generate authored document schemas

## [0.45.3](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.45.2...phoxal-manifest-v0.45.3) - 2026-07-31

### Added

- *(manifest)* declare runtime build requirements ([#366](https://github.com/phoxal/framework/pull/366))

## [0.45.1](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.45.0...phoxal-manifest-v0.45.1) - 2026-07-30

### Added

- *(model)* expose canonical component structures ([#362](https://github.com/phoxal/framework/pull/362))

## [0.45.0](https://github.com/phoxal/framework/compare/phoxal-manifest-v0.44.0...phoxal-manifest-v0.45.0) - 2026-07-30

### Other

- [**breaking**] split framework ownership boundaries ([#360](https://github.com/phoxal/framework/pull/360))
