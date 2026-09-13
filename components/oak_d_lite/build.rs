fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(
        &["proto/phoxal/component/oak_d_lite/v1/oak_d_lite.proto"],
        &["proto"],
    )
}
