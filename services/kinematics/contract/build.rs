fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos_with_dependencies(
        &["proto/phoxal/kinematics/v1/kinematics.proto"],
        &["proto"],
        &[phoxal_build::DependencyDescriptor::new(
            "phoxal-robotics",
            phoxal_robotics::FILE_DESCRIPTOR_SET,
        )],
        &[(".phoxal.robotics.v1", "::phoxal_robotics")],
    )
}
