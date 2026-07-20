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
