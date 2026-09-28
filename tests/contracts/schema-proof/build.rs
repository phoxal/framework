//! Compiles the independent Protobuf reference used by the interop proof.

use std::path::PathBuf;

fn main() -> Result<(), phoxal::build::Error> {
    // The robotics import resolves against the build helper's packaged
    // vocabulary exactly as an ordinary consumer's build script would; it is
    // compiled as an owned file so the generated reference carries its own
    // copy of the imported types. The geometry, battery, and lidar
    // references are this fixture's own mirrors of the SDK's authored
    // shapes, so protoc compiles the same wire contract the SDK declares.
    let manifest = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let reference = manifest.join("api/proof/v1/reference.proto");
    let robotics = phoxal::build::include_dir().join("phoxal/robotics/v1/robotics.proto");
    let geometry = manifest.join("api/phoxal/geometry/v1/geometry.proto");
    let battery = manifest.join("api/phoxal/component/battery/v1/battery.proto");
    let lidar = manifest.join("api/phoxal/component/lidar/v1/lidar.proto");
    phoxal::build::compile_protos_with_output(
        &[reference, robotics, geometry, battery, lidar],
        &[manifest.join("api")],
        "reference.bin",
    )
}
