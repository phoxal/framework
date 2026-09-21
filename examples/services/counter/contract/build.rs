fn main() -> Result<(), phoxal::build::Error> {
    phoxal::build::compile_protos(&["proto/example/counter/v1/counter.proto"], &["proto"])
}
