fn main() -> Result<(), phoxal::build::Error> {
    phoxal::build::compile_protos(
        &["proto/phoxal/component/bno085/v1/bno085.proto"],
        &["proto"],
    )
}
