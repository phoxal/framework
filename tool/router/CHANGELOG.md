# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Fixed

- *(release)* Skip the GitHub-corrupted, permanently uncreatable
  `phoxal-tool-router-v0.1.5` ref by seeding the release ledger at v0.1.6; the
  next release advances to a fresh tag.

## [0.1.5](https://github.com/phoxal/framework/releases/tag/phoxal-tool-router-v0.1.5) - 2026-07-10

### Added

- *(04)* bus log layer + tool-router and tool-joypad artifacts ([#147](https://github.com/phoxal/framework/pull/147))

### Fixed

- *(11)* webots-rs 0.1.3 - c_char portability unblocks the simulator aarch64 asset ([#163](https://github.com/phoxal/framework/pull/163))
- *(01)* simulator publish flag + discovery guard against cargo-level publish=false ([#162](https://github.com/phoxal/framework/pull/162))
- *(01)* draft-then-publish artifact releases for immutable-releases repos ([#161](https://github.com/phoxal/framework/pull/161))
- *(01)* make git_only artifact releases actually release ([#159](https://github.com/phoxal/framework/pull/159))

### Other

- delete old authoring model + emit-apis; run_v2 -> run (Cleanup)
- *(release)* bump changed artifacts ([#167](https://github.com/phoxal/framework/pull/167))
- *(release)* artifacts leave release-plz - xtask cuts tags/drafts, release-pr drops from ~100min to minutes ([#165](https://github.com/phoxal/framework/pull/165))
- *(release)* release ([#155](https://github.com/phoxal/framework/pull/155))
- *(01)* activate artifact releases - baseline bump ([#157](https://github.com/phoxal/framework/pull/157))

## [0.1.0](https://github.com/phoxal/framework/releases/tag/phoxal-tool-router-v0.1.0) - 2026-07-04

### Added

- *(04)* bus log layer + tool-router and tool-joypad artifacts ([#147](https://github.com/phoxal/framework/pull/147))
