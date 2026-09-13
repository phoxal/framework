fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(&["proto/example/counter/v1/counter.proto"], &["proto"])
}
