fn main() -> Result<(), phoxal_build::Error> {
    let includes = [
        std::path::PathBuf::from("proto"),
        phoxal_robotics::proto_include_dir(),
    ];
    phoxal_build::compile_protos_with_externs(
        &["proto/phoxal/kinematics/v1/kinematics.proto"],
        &includes,
        &[(".phoxal.robotics.v1", "::phoxal_robotics")],
    )
}
