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
    if std::env::var_os("CARGO_FEATURE_ROBOTICS").is_some() {
        // The build helper owns the canonical robotics schema so the SDK and
        // package-local generation compile one identical descriptor source.
        let builtin = phoxal_build::include_dir();
        phoxal_build::compile_protos_with_output(
            &[builtin.join("phoxal/robotics/v1/robotics.proto")],
            &[builtin],
            "robotics-descriptors.bin",
        )?;
    }
    Ok(())
}
