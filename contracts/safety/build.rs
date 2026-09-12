fn main() -> Result<(), phoxal_build::Error> {
    let includes = [
        std::path::PathBuf::from("proto"),
        phoxal_motion::proto_include_dir(),
        phoxal_world::proto_include_dir(),
        phoxal_kinematics::proto_include_dir(),
        phoxal_robotics::proto_include_dir(),
    ];
    phoxal_build::compile_protos_with_externs(
        &["proto/phoxal/safety/v1/safety.proto"],
        &includes,
        &[
            (".phoxal.motion.v1", "::phoxal_motion"),
            (".phoxal.world.v1", "::phoxal_world"),
        ],
    )
}
