//! Cargo train anchor for the `phoxal-component-vl53l1x` package.
//!
//! This crate has no public API. Its only purpose is to give the package a
//! library target so `cargo metadata` can resolve it as a dependency and
//! report where Cargo extracted its assets (`component.yaml`,
//! `simulation.yaml`, `structure.urdf`). The driver binary lives in
//! `src/main.rs` and `src/vl53l1x.rs`; nothing here re-exports it.
