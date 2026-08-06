//! Reading the finalized runtime bundle a participant was launched against.
//!
//! A participant normally never touches this module: the runner loads the
//! bundle named by `PHOXAL_BUNDLE_ROOT` and binds the model and assets onto
//! `SetupContext`. It is public because the same loader is the one a host tool
//! embedding the framework uses to inspect a bundle it did not launch.
//!
//! Whole-bundle serving (`BundleResolver`, which reaches `bin/`) is deliberately
//! absent here: that is a supervisor capability, and it lives in
//! `phoxal_manifest::bundle` where the supervisor already looks.

pub use phoxal_manifest::bundle::{BundleError, FinalizedBundle};
