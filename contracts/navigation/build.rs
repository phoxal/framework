fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(&["proto/phoxal/navigation/v1/navigation.proto"], &["proto"])
}
