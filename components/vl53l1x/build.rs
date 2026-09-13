fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(
        &["proto/phoxal/component/vl53l1x/v1/vl53l1x.proto"],
        &["proto"],
    )
}
