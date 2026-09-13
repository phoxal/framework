fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(
        &["proto/phoxal/component/zed_f9p/v1/zed_f9p.proto"],
        &["proto"],
    )
}
