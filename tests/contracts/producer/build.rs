fn main() {
    if let Err(error) = phoxal::build::compile_protos(
        &[
            "proto/example/inspection/v1/messages.proto",
            "proto/example/inspection/v1/inspection.proto",
        ],
        &["proto"],
    ) {
        eprintln!("contract generation failed: {error}");
        std::process::exit(1);
    }
}
