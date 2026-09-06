//! The one baseline the checker still reads through the retired multi-crate
//! topology.
//!
//! The framework used to publish its process/wire surface from five library
//! packages and its authored-source reader from a sixth. It now publishes one
//! library, so a baseline resolves from `phoxal` alone. That leaves exactly one
//! comparison in an awkward position: the release that *performs* the merge is
//! compared against the last train that predates it, and on that train the
//! records this workspace states from one crate were stated from six.
//!
//! So a baseline below [`topology_floor`] is read from the packages that
//! actually carried it, at the same version, and their records are unioned.
//! Record identity carries no crate name, so a record that moved from
//! `phoxal-protocol` into `phoxal` is the same record on both sides and the
//! comparison sees the merge for what it is: no wire change.
//!
//! **This whole module is deletable** once a `0.66.x` `phoxal` is published:
//! from then on every resolvable baseline is at or above the floor and nothing
//! calls in here. Deleting it is `rm xtask/src/legacy.rs`, dropping `mod
//! legacy;` from the runner, and taking the two `Side` branches that consult it
//! with the module.

use semver::Version;

/// The first framework train published as one library.
///
/// Stated as a version rather than as a flag because that is what the registry
/// answers with: whether a baseline predates the merge is a fact about the
/// version that resolved, not a mode the caller selects.
pub(crate) fn topology_floor() -> Version {
    Version::new(0, 66, 0)
}

/// Whether a resolved baseline predates the single-crate topology and must
/// therefore be read through the packages that carried it.
pub(crate) fn precedes_the_single_crate_topology(baseline: &Version) -> bool {
    *baseline < topology_floor()
}

/// The packages that carried the process/wire surface before the merge.
///
/// Read at the exact resolved version, all five or none: a train published them
/// as one set, so a version one of them does not carry is a baseline that never
/// existed rather than a smaller surface. Cargo enforces that by failing to
/// resolve the pin.
pub(crate) const CONTRACT_CARRIERS: [&str; 5] = [
    "phoxal",
    "phoxal-protocol",
    "phoxal-bus",
    "phoxal-bundle",
    "phoxal-runtime-contract",
];

/// The package that carried the authored-source reader before the merge, and
/// the crate path the probe program reaches its entry point through.
///
/// The reader itself moved into `phoxal::authoring`, so the authored-source leg
/// would otherwise report the merge train as having no probe entry at all - the
/// one train nobody would be holding the source language to. Naming the old
/// package here keeps that leg running across the cutover.
pub(crate) const SOURCE_READER_PACKAGE: &str = "phoxal-manifest";

/// The crate path [`SOURCE_READER_PACKAGE`] is named by in Rust source.
pub(crate) const SOURCE_READER_PATH: &str = "phoxal_manifest";

#[cfg(test)]
mod tests {
    use super::*;

    /// The floor is the release that performs the merge, so the last train
    /// before it is legacy and the merge train itself is not.
    #[test]
    fn the_floor_separates_the_merge_train_from_the_one_before_it() {
        assert!(precedes_the_single_crate_topology(&Version::new(0, 65, 0)));
        assert!(precedes_the_single_crate_topology(&Version::new(0, 65, 9)));
        assert!(!precedes_the_single_crate_topology(&Version::new(0, 66, 0)));
        assert!(!precedes_the_single_crate_topology(&Version::new(0, 67, 0)));
        assert!(!precedes_the_single_crate_topology(&Version::new(1, 0, 0)));
    }

    /// The one carrier this workspace still publishes is among the legacy set,
    /// which is what makes the union comparable to the workspace side at all.
    #[test]
    fn the_surviving_carrier_is_part_of_the_legacy_set() {
        assert!(CONTRACT_CARRIERS.contains(&crate::surface::CONTRACT_CRATE));
    }
}
