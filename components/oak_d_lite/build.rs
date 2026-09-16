fn main() -> Result<(), phoxal::build::Error> {
    phoxal::build::compile_protos(
        &["proto/phoxal/component/oak_d_lite/v1/oak_d_lite.proto"],
        &["proto"],
    )
}
