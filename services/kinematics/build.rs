fn main() -> Result<(), phoxal::build::Error> {
    phoxal::build::compile_protos_with_dependencies(
        &["proto/phoxal/kinematics/v1/kinematics.proto"],
        &["proto"],
        &[phoxal::build::DependencyDescriptor::new(
            "phoxal-robotics",
            phoxal_robotics::FILE_DESCRIPTOR_SET,
        )],
        &[(".phoxal.robotics.v1", "::phoxal_robotics")],
    )
}
