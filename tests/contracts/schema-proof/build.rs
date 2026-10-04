//! Compiles test-owned independent Protobuf references, never production inputs.
fn main() -> Result<(), Box<dyn std::error::Error>> {
    let files = [
        "api/proof/v1/reference.proto",
        "api/phoxal/robotics/v1/robotics.proto",
        "api/phoxal/geometry/v1/geometry.proto",
        "api/phoxal/component/battery/v1/battery.proto",
        "api/phoxal/component/lidar/v1/lidar.proto",
        "api/phoxal/bootstrap/v1/bootstrap.proto",
        "api/phoxal/session/v1/session.proto",
        "api/phoxal/execution/v1/execution.proto",
        "api/phoxal/simulation/v1/simulation.proto",
    ];
    let google = protoc_bin_vendored::include_path()?;
    let sources = files
        .iter()
        .map(std::path::PathBuf::from)
        .chain(std::iter::once(google.join("google/protobuf/empty.proto")))
        .collect::<Vec<_>>();
    let mut config = prost_build::Config::new();
    config.compile_well_known_types();
    config
        .protoc_executable(protoc_bin_vendored::protoc_bin_path()?)
        .file_descriptor_set_path(
            std::path::PathBuf::from(std::env::var("OUT_DIR")?).join("reference.bin"),
        )
        .enable_type_names();
    config.compile_protos(&sources, &[std::path::PathBuf::from("api"), google])?;
    for file in files {
        println!("cargo:rerun-if-changed={file}");
    }
    Ok(())
}
