fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(&["proto/phoxal/world/v1/world.proto"], &["proto"])
}
