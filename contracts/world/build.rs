fn main() -> Result<(), phoxal_build::Error> {
    let includes = [
        std::path::PathBuf::from("proto"),
        phoxal_kinematics::proto_include_dir(),
        phoxal_robotics::proto_include_dir(),
    ];
    phoxal_build::compile_protos_with_externs(
        &["proto/phoxal/world/v1/world.proto"],
        &includes,
        &[(".phoxal.kinematics.v1", "::phoxal_kinematics")],
    )
}
