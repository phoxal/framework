fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(&["proto/phoxal/safety/v1/safety.proto"], &["proto"])
}
