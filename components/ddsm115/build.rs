fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos_with_dependencies(
        &["proto/phoxal/component/ddsm115/v1/ddsm115.proto"],
        &["proto"],
        &[phoxal_build::DependencyDescriptor::new(
            "phoxal-robotics",
            phoxal_robotics::FILE_DESCRIPTOR_SET,
        )],
        &[(".phoxal.robotics.v1", "::phoxal_robotics")],
    )
}
