fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(
        &[
            "proto/phoxal/bootstrap/v1/bootstrap.proto",
            "proto/phoxal/session/v1/session.proto",
            "proto/phoxal/execution/v1/execution.proto",
            "proto/phoxal/simulation/v1/simulation.proto",
        ],
        &["proto"],
    )?;
    phoxal_build::compile_protos_with_output(
        &["proto/phoxal/robotics/v1/robotics.proto"],
        &["proto"],
        "phoxal-robotics-descriptors.bin",
    )
}
