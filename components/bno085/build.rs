fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(
        &["proto/phoxal/component/bno085/v1/bno085.proto"],
        &["proto"],
    )
}
