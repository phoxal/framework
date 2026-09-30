fn main() -> Result<(), phoxal_build::Error> {
    println!(
        "cargo:rustc-env=PHOXAL_API_GENERATOR_VERSION={}",
        phoxal_build::GENERATOR_VERSION
    );
    phoxal_build::compile_protos(
        &[
            "proto/phoxal/bootstrap/v1/bootstrap.proto",
            "proto/phoxal/session/v1/session.proto",
            "proto/phoxal/execution/v1/execution.proto",
            "proto/phoxal/simulation/v1/simulation.proto",
        ],
        &["proto"],
    )?;
    Ok(())
}
