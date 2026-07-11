# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.19.7](https://github.com/phoxal/framework/compare/phoxal-service-asset-v0.19.6...phoxal-service-asset-v0.19.7) - 2026-07-11

### Fixed

- *(release)* reconcile asset+router versions past the GH013-cursed tags ([#225](https://github.com/phoxal/framework/pull/225))
- *(release)* skip two GitHub-corrupted baseline tags (service-asset v0.19.6, tool-router v0.1.5) ([#218](https://github.com/phoxal/framework/pull/218))

### Fixed

- *(release)* Skip the GitHub-corrupted, permanently uncreatable
  `phoxal-service-asset-v0.19.6` ref by seeding the release ledger at v0.19.7;
  the next release advances to a fresh tag.

## [0.19.6](https://github.com/phoxal/framework/releases/tag/phoxal-service-asset-v0.19.6) - 2026-07-10

### Added

- *(18)* the clap launch contract + robot_root rename ([#145](https://github.com/phoxal/framework/pull/145))
- *(15,16)* finish authoring taxonomy + per-contract schema_id (framework) ([#131](https://github.com/phoxal/framework/pull/131))

### Fixed

- *(11)* webots-rs 0.1.3 - c_char portability unblocks the simulator aarch64 asset ([#163](https://github.com/phoxal/framework/pull/163))
- *(01)* simulator publish flag + discovery guard against cargo-level publish=false ([#162](https://github.com/phoxal/framework/pull/162))
- *(01)* draft-then-publish artifact releases for immutable-releases repos ([#161](https://github.com/phoxal/framework/pull/161))
- *(01)* make git_only artifact releases actually release ([#159](https://github.com/phoxal/framework/pull/159))

### Other

- delete old authoring model + emit-apis; run_v2 -> run (Cleanup)
- convert remaining 24 participants to new authoring model (P-convert)
- *(release)* bump changed artifacts ([#167](https://github.com/phoxal/framework/pull/167))
- *(release)* artifacts leave release-plz - xtask cuts tags/drafts, release-pr drops from ~100min to minutes ([#165](https://github.com/phoxal/framework/pull/165))
- *(release)* release ([#155](https://github.com/phoxal/framework/pull/155))
- *(01)* activate artifact releases - baseline bump ([#157](https://github.com/phoxal/framework/pull/157))

## [0.19.1](https://github.com/phoxal/framework/releases/tag/phoxal-service-asset-v0.19.1) - 2026-07-04

### Added

- *(18)* the clap launch contract + robot_root rename ([#145](https://github.com/phoxal/framework/pull/145))
- *(15,16)* finish authoring taxonomy + per-contract schema_id (framework) ([#131](https://github.com/phoxal/framework/pull/131))
