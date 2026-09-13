fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos_with_dependencies(
        &["proto/phoxal/safety/v1/safety.proto"],
        &["proto"],
        &[
            phoxal_build::DependencyDescriptor::new(
                "phoxal-robotics",
                phoxal_robotics::FILE_DESCRIPTOR_SET,
            ),
            phoxal_build::DependencyDescriptor::new(
                "phoxal-motion",
                phoxal_motion::FILE_DESCRIPTOR_SET,
            ),
            phoxal_build::DependencyDescriptor::new(
                "phoxal-world",
                phoxal_world::FILE_DESCRIPTOR_SET,
            ),
        ],
        &[
            (".phoxal.robotics.v1", "::phoxal_robotics"),
            (".phoxal.motion.v1", "::phoxal_motion"),
            (".phoxal.world.v1", "::phoxal_world"),
        ],
    )
}
