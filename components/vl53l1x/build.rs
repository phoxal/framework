fn main() -> Result<(), phoxal::build::Error> {
    phoxal::build::compile_protos_with_dependencies(
        &["proto/phoxal/component/vl53l1x/v1/vl53l1x.proto"],
        &["proto"],
        &[phoxal::build::DependencyDescriptor::new(
            "phoxal-robotics",
            phoxal_robotics::FILE_DESCRIPTOR_SET,
        )],
        &[(".phoxal.robotics.v1", "::phoxal_robotics")],
    )
}
