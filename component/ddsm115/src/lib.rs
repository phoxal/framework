//! Cargo train anchor for the `phoxal-component-ddsm115` package.
//!
//! This crate has no public API. Its only purpose is to give the package a
//! library target so `cargo metadata` can resolve it as a dependency and
//! report where Cargo extracted its assets (`component.yaml`,
//! `simulation.yaml`, `structure.urdf`, `meshes/`). The driver binary lives
//! in `src/main.rs` and `src/ddsm115.rs`; nothing here re-exports it.
