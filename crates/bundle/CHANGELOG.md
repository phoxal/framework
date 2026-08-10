# Changelog

All notable changes to `phoxal-bundle` are recorded by the workspace release
train.

## Unreleased

## [0.57.0](https://github.com/phoxal/framework/compare/phoxal-bundle-v0.56.2...phoxal-bundle-v0.57.0) - 2026-08-10

### Added

- [**breaking**] make the framework train version the single compatibility identity ([#429](https://github.com/phoxal/framework/pull/429))

## [0.56.2](https://github.com/phoxal/framework/compare/phoxal-bundle-v0.56.1...phoxal-bundle-v0.56.2) - 2026-08-10

### Added

- *(supervisor)* add execution control contract

## [0.56.1](https://github.com/phoxal/framework/compare/phoxal-bundle-v0.56.0...phoxal-bundle-v0.56.1) - 2026-08-10

### Other

- enforce final framework hygiene ([#423](https://github.com/phoxal/framework/pull/423))

## [0.56.0](https://github.com/phoxal/framework/compare/phoxal-bundle-v0.55.0...phoxal-bundle-v0.56.0) - 2026-08-09

### Added

- *(api)* [**breaking**] simplify modular Robot API authoring ([#422](https://github.com/phoxal/framework/pull/422))
- *(model)* [**breaking**] preserve authored safety runtime truth ([#420](https://github.com/phoxal/framework/pull/420))
- *(bundle)* [**breaking**] separate runtime artifacts from participant instances ([#418](https://github.com/phoxal/framework/pull/418))
- *(api)* [**breaking**] enforce control wire-state invariants
- *(bundle)* [**breaking**] persist validated runtime documents

### Fixed

- *(participant)* [**breaking**] complete lifecycle ownership guarantees ([#416](https://github.com/phoxal/framework/pull/416))
- *(runtime)* [**breaking**] enforce static drive topology and motor modes

### Other

- *(bundle)* cover atomic publication races ([#414](https://github.com/phoxal/framework/pull/414))

- Add the schema-tagged persisted runtime document, integrity-checked bundle
  reader/writer, and exact participant selection API.
