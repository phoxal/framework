# Changelog

All notable changes documented here. The format follows
[Keep a Changelog](https://keepachangelog.com/) and the project follows
[Semantic Versioning](https://semver.org/).

## [0.5.0](https://github.com/phoxal/framework/releases/tag/v0.5.0) - 2026-06-11


### Added

- *(bus)* Migrate the framework to a typed fluent-topic bus (#55)

### CI

- Reuse runtime-image binaries for Linux release tarballs (#53)
- Use the shared rust-ci reusable workflow (#54)

### Fixed

- *(release)* Track phoxal-macros via the workspace version (#56)

### Refactored

- *(bus)* Replace phoxal-macros proc-macro with a macro_rules! tree (#58) [**breaking**]

## [0.4.0](https://github.com/phoxal/framework/releases/tag/v0.4.0) - 2026-06-09


### Added

- *(engine)* Immediate queries over committed read views (#42)
- *(core-robot)* Optional directory selector for git component sources (#46)
- *(core-component)* Typed optional gtin on the component schema (#47)
- *(api-mission)* Split arrival tolerance from execution budget (#49)
- *(core-engine)* Engine-owned decision-logging contract (#50)

### CI

- *(release)* Add runtime-image release verification gate (#31) (#32)
- *(release)* Make release tag handling explicit and retry-safe (#36)
- Adopt shared reusable release workflows (#37)
- Enforce Conventional Commit PR titles (#38)
- Repoint reusable workflows to public phoxal/.github (#39)
- Gate release on the release-prep branch, not the PR title (#40)
- Gate releases on the release/ branch prefix (#41)
- Publish the phoxal crate to crates.io on release (#52)

### Documentation

- Point runtime READMEs at docs/BLUEPRINT after codex/ -> docs/ rename (#33)
- Re-home v1 contract, convention, and validation docs (#44)
- Drop stale deploy-descriptor reference in localize_backend (#45)

### Refactored

- *(core-robot)* Remove redundant KinematicKind discriminator (#48)
- Collapse the framework into one phoxal crate (#51)

### Style

- Cargo fmt --all (#34)

## [0.3.0](https://github.com/phoxal/framework/releases/tag/v0.3.0) - 2026-05-29


### Added

- *(robot)* Add optional network section + infra/router README

### Documentation

- *(infra/router)* Document zenoh image override + rename to DEFAULT_ZENOH_IMAGE

### Other

- *(infra)* Move Dockerfile.runtime to infra/runtime/
- *(framework)* Regroup workspace into infra/api/core/runtime/validation; rename packages to match

### Refactored

- *(router)* Drop in-tree zenoh router crate
- *(robot)* Drop sim section from robot.yaml schema

## [0.2.0](https://github.com/phoxal/framework/releases/tag/v0.2.0) - 2026-05-28


### Added

- *(release)* Publish per-runtime native binaries alongside docker images

### Fixed

- *(release)* Drop git push tag step (gh release create --target main handles both via API; bypasses workflow-touching commit restriction)

## [0.1.0](https://github.com/phoxal/framework/releases/tag/v0.1.0) - 2026-05-28


### Added

- *(ci)* Bootstrap homegrown release flow

### CI

- *(release)* Replace release-plz with homegrown release-prep PR + matrix release
- *(release)* Keep release-prep body out of PR diff
- *(release-prep)* Skip when Cargo.toml is ahead of last tag (release in flight); cliff ignores 'release:' commits

## [0.0.0-dev](https://github.com/phoxal/framework/releases/tag/v0.0.0-dev) - 2026-05-28


### Added

- Integrate ORB-SLAM3 backend with robot-localize runtime
- *(utils-robot)* Single Robot struct + new robot.yaml schema

### CI

- Wire docker images + release-plz + GH release for the runtime workspace

### Documentation

- Drop stale robot-framework / cargo xtask references

### Fixed

- *(workspace)* Add [patch] sections for transitive-git phoxal-* deps
- *(tests)* Relocate fixture/ into framework; fix post-flatten paths
- *(ci)* Images.yml triggers on phoxal-bus-v* (release-plz uses per-crate tag pattern)
- *(ci)* Images.yml strips phoxal-bus-v prefix to get the workspace version

### Other

- *(license)* Switch workspace to AGPL-3.0-only
- Bootstrap framework workspace
- Ignore target/ and editor cruft
- *(workspace)* Drop phoxal-simulator-api workspace dep
- *(version)* Workspace → 0.0.0-dev for the pre-release period
- Release v0.0.0-dev

### Refactored

- *(workspace)* Carve members into future-repo subdirs
- *(engine)* Fold phoxal-utils-conventions into phoxal-engine
- *(api)* Introduce pub mod v1 in every phoxal-*-api crate
- *(framework)* Delete dead RuntimeBudget; adopt v1 dispatcher in utils-robot; sweep dead code
- *(tests)* Annotate live-bus tests with #[serial]; tidy localize selector tempdirs
- *(engine)* Own SimulationClock; drop engine→simulator-api dep edge
- *(workspace)* Drop utils- prefix; merge scenario crates; structure runtimes/<name>/{api,runtime}/

### Tests

- *(fixture)* Plan_robot.yaml uses tag: main for real catalog repos
- *(safety)* Replace ignored robot-v1 test with fixture-driven coverage

