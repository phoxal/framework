//! Cargo package anchor for the `phoxal-supervisor` executable.
//!
//! Robot manifests depend on the supervisor package so Cargo resolves and
//! locks the exact executable version alongside the framework runtime.
//! The supervisor implementation is intentionally private to its binary
//! modules and this library exposes no API.
