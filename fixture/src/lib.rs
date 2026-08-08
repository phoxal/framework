//! Staging for the workspace fixture robot.
//!
//! The fixture lives beside this crate as authored documents only - `robot.yaml`,
//! `component.yaml`, `simulation.yaml`, URDF and meshes. Turning those into
//! something [`FinalizedBundle`] can load is what a test *about document
//! loading* needs, and doing it once here means there is a single answer to
//! what the fixture robot *is*.
//!
//! This is not a general way to get a [`Robot`]. A test that needs a robot
//! model and does not care where it came from should state the robot it wants
//! with [`phoxal_model::RobotBuilder`], which composes a validated model in
//! memory: no filesystem, no compiler, and no incidental properties of a
//! document the test did not write. Use this crate only when the authored
//! document, the staged tree or the bundle around it is itself the thing under
//! test.
//!
//! Finalization itself belongs to `phoxal-cli`. The two edits it makes to the
//! authored document are small enough to reproduce here, so no finalized bundle
//! has to be checked in anywhere.
//!
//! This crate is never published: the paths it resolves are relative to this
//! repository's layout and mean nothing outside it.

use std::fs;
use std::path::Path;

use phoxal_manifest::bundle::FinalizedBundle;
use phoxal_model::Robot;
use tempfile::TempDir;

/// The component types the fixture robot declares.
const COMPONENT_TYPES: [&str; 5] = [
    "camera_rgbd_640x480",
    "drive_motor",
    "gnss",
    "imu",
    "range_tof",
];

/// The authored fixture tree, resolved against this crate rather than the
/// caller's.
///
/// Every consumer used to resolve this from its own manifest directory, which
/// made the path depend on how deeply the calling crate happened to be nested.
fn authored_root() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

/// Stage the fixture robot into a temporary bundle root.
///
/// The returned [`TempDir`] owns the staged tree and deletes it on drop, so a
/// caller must hold it for as long as it reads the bundle.
///
/// Two edits are applied to the authored `robot.yaml` as it is staged, which is
/// exactly what finalization does: the clock is pinned to `real` so the loaded
/// robot does not wait on a simulation world authority, and the structure
/// reference is repointed at its staged location under `assets/`.
///
/// # Panics
///
/// Panics if the fixture cannot be staged. This is test-support code, and a
/// missing or malformed fixture is a broken checkout rather than a condition a
/// caller could do anything about.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "every input is a document committed beside this crate, so a failure here is a broken checkout and the panic is the report"
)]
pub fn staged_bundle() -> TempDir {
    let fixture = authored_root();
    let project = fixture.join("robot/rgbd-imu-diff-drive");
    let bundle = tempfile::tempdir().expect("a staging directory");
    let assets = bundle.path().join("assets");

    fs::create_dir_all(assets.join("robot")).expect("the staged robot asset directory");
    fs::copy(
        project.join("structure.urdf"),
        assets.join("robot/structure.urdf"),
    )
    .expect("the robot structure is part of the fixture");

    for component_type in COMPONENT_TYPES {
        let source = fixture.join("component").join(component_type);
        let staged = assets.join("components").join(component_type);
        fs::create_dir_all(&staged).expect("the staged component directory");
        for document in ["component.yaml", "simulation.yaml", "structure.urdf"] {
            fs::copy(source.join(document), staged.join(document))
                .expect("every fixture component carries all three documents");
        }
        for mesh in fs::read_dir(source.join("meshes")).into_iter().flatten() {
            let mesh = mesh.expect("a readable fixture mesh entry");
            fs::create_dir_all(staged.join("meshes")).expect("the staged mesh directory");
            fs::copy(mesh.path(), staged.join("meshes").join(mesh.file_name()))
                .expect("a readable fixture mesh");
        }
    }

    let authored = fs::read_to_string(project.join("robot.yaml")).expect("the fixture robot.yaml");
    fs::write(
        bundle.path().join("robot.yaml"),
        authored
            .replacen(
                "schema: phoxal/robot/v0",
                "schema: phoxal/robot/v0\nclock: real",
                1,
            )
            .replacen(
                "structure: structure.urdf",
                "structure: robot/structure.urdf",
                1,
            ),
    )
    .expect("the staged robot.yaml");

    bundle
}

/// The canonical fixture robot, staged and loaded.
///
/// This is the whole authored-document path run end to end, so it answers "do
/// these documents still compile into a model" and nothing else. A test that
/// wants a robot to assert on states one with
/// [`phoxal_model::RobotBuilder`] instead.
///
/// The staging directory is dropped before returning, which is safe because the
/// bundle has already been read into the returned value.
///
/// # Panics
///
/// Panics if the staged fixture does not load, which means the fixture
/// documents and the compiler have gone out of step.
#[must_use]
#[expect(
    clippy::expect_used,
    reason = "the fixture documents and the compiler that reads them are both committed here, so a load failure is a broken checkout and the panic is the report"
)]
pub fn robot() -> Robot {
    let bundle = staged_bundle();
    FinalizedBundle::load(bundle.path())
        .expect("the staged bundle must load")
        .into_robot()
}

#[cfg(test)]
mod tests {
    use super::{COMPONENT_TYPES, robot, staged_bundle};

    #[test]
    fn the_staged_bundle_carries_the_robot_and_every_component() {
        let bundle = staged_bundle();
        let root = bundle.path();
        assert!(root.join("robot.yaml").is_file());
        assert!(root.join("assets/robot/structure.urdf").is_file());
        for component_type in COMPONENT_TYPES {
            assert!(
                root.join("assets/components")
                    .join(component_type)
                    .join("component.yaml")
                    .is_file(),
                "{component_type} must be staged"
            );
        }
    }

    /// The staged clock override is what lets a test load the fixture without a
    /// simulation world authority, so it is worth pinning rather than trusting.
    #[test]
    fn the_fixture_robot_loads_on_the_real_clock() {
        assert_eq!(robot().clock(), phoxal_model::Clock::Real);
    }
}
