fn main() -> Result<(), phoxal::build::Error> {
    phoxal::build::compile_protos_with_dependencies(
        &["proto/phoxal/safety/v1/safety.proto"],
        &["proto"],
        &[
            phoxal::build::DependencyDescriptor::new(
                "phoxal-robotics",
                phoxal_robotics::FILE_DESCRIPTOR_SET,
            ),
            phoxal::build::DependencyDescriptor::new(
                "phoxal-service-motion",
                phoxal_service_motion::FILE_DESCRIPTOR_SET,
            ),
            phoxal::build::DependencyDescriptor::new(
                "phoxal-service-world",
                phoxal_service_world::FILE_DESCRIPTOR_SET,
            ),
        ],
        &[
            (".phoxal.robotics.v1", "::phoxal_robotics"),
            (".phoxal.motion.v1", "::phoxal_service_motion"),
            (".phoxal.world.v1", "::phoxal_service_world"),
        ],
    )
}
