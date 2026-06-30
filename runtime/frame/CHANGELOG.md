# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

## [0.19.1](https://github.com/phoxal/framework/releases/tag/phoxal-runtime-frame-v0.19.1) - 2026-06-30

### Added

- *(00)* L2 owner capability + close raw-bus hole ([#120](https://github.com/phoxal/framework/pull/120))
- *(00)* L1 compile-time owner/client enforcement via side-branded topics ([#118](https://github.com/phoxal/framework/pull/118))
- reconcile framework to the rewrite spec + full rustdoc (plan-vs-code audit) ([#114](https://github.com/phoxal/framework/pull/114))
- *(api)* [**breaking**] nested self-contained node grammar for phoxal_api_tree! ([#112](https://github.com/phoxal/framework/pull/112))
- [**breaking**] recover all 18 official runtimes into a single curated y2026_1 ([#110](https://github.com/phoxal/framework/pull/110))
- framework rewrite — greenfield engine + macros (vertical slice + query serving) ([#82](https://github.com/phoxal/framework/pull/82))
- *(api)* [**breaking**] domain-first versioned contract enums; drop schema_version axis ([#69](https://github.com/phoxal/framework/pull/69)) ([#70](https://github.com/phoxal/framework/pull/70))
- *(bus)* migrate the framework to a typed fluent-topic bus ([#55](https://github.com/phoxal/framework/pull/55))
- *(engine)* immediate queries over committed read views ([#42](https://github.com/phoxal/framework/pull/42))

### Other

- *(release)* per-artifact release pipeline with release-plz (#01-B) ([#125](https://github.com/phoxal/framework/pull/125))
- *(release)* decouple per-crate versions + caret pins (plan #01 foundation) ([#124](https://github.com/phoxal/framework/pull/124))
- *(00)* drop phoxal::api facade, repoint to phoxal_api::y2026_1 ([#119](https://github.com/phoxal/framework/pull/119))
- *(runtime)* unify tokio dependency across runtimes ([#74](https://github.com/phoxal/framework/pull/74))
- collapse the framework into one phoxal crate ([#51](https://github.com/phoxal/framework/pull/51))
- cargo fmt --all ([#34](https://github.com/phoxal/framework/pull/34))
- *(framework)* regroup workspace into infra/api/core/runtime/validation; rename packages to match
