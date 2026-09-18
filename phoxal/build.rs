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
    // The shared robotics vocabulary is gated behind the `robotics` feature
    // so consumers that only need the inert port surface do not pay for its
    // codegen. Cargo exports one `CARGO_FEATURE_<name>` env var per enabled
    // feature; checking it here keeps the build-time generation in lockstep
    // with the `pub mod robotics;` declaration in `src/lib.rs`.
    if std::env::var_os("CARGO_FEATURE_ROBOTICS").is_some() {
        phoxal_build::compile_protos_with_output(
            &["proto/phoxal/robotics/v1/robotics.proto"],
            &["proto"],
            "phoxal-robotics-descriptors.bin",
        )?;
    }
    Ok(())
}
