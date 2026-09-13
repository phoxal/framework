fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos_with_dependencies(
        &["proto/phoxal/world/v1/world.proto"],
        &["proto"],
        &[phoxal_build::DependencyDescriptor::new(
            "phoxal-kinematics",
            phoxal_kinematics::FILE_DESCRIPTOR_SET,
        )],
        &[(".phoxal.kinematics.v1", "::phoxal_kinematics")],
    )
}
