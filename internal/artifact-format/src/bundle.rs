//! Bundle manifest and supporting records.
//!
//! Owns `BundleManifest` and its inert DTO closure, plus
//! `BundleProvenance` family and the pure `digest_source_files` algorithm.
//! Bundle assembly, staging, publication, and Cargo execution remain in
//! `phoxal-project`'s tool layer.
