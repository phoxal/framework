fn main() -> Result<(), phoxal::build::Error> {
    phoxal::build::compile_protos_with_dependencies(
        &["proto/phoxal/world/v1/world.proto"],
        &["proto"],
        &[phoxal::build::DependencyDescriptor::new(
            "phoxal-service-kinematics",
            phoxal_service_kinematics::FILE_DESCRIPTOR_SET,
        )],
        &[(".phoxal.kinematics.v1", "::phoxal_service_kinematics")],
    )
}
