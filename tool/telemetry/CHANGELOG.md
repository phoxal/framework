# Changelog

All notable changes to this project will be documented in this file.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.0.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- Retain five minutes of portable per-participant runtime-performance rollups
  and expose bounded snapshot/follow recovery under `v1::tool::runtime`.
- Bound runtime history by bytes and records, clamp retained identities/rows,
  and disclose every truncation or overflow through the stable record shape.
- Re-aggregate duplicate runtime keys before row limits and clamp every
  retained rate to a finite, saturating value.
- Retain bounded, capability-aware device samples from the per-root
  `phoxal-tool-device` publisher and expose snapshot/follow recovery.

### Removed

- Move operating-system sampling out of tool-telemetry so this retention tool
  no longer acts as a site-scoped device authority.
