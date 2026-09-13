fn main() {
    if let Err(error) = phoxal_build::compile_protos(
        &["proto/phoxal/fixture/hardware/v1/hardware.proto"],
        &["proto"],
    ) {
        eprintln!("hardware fixture contract generation failed: {error}");
        std::process::exit(1);
    }
}
