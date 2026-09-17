fn main() -> Result<(), phoxal::build::Error> {
    phoxal::build::compile_protos(&["proto/phoxal/navigation/v1/navigation.proto"], &["proto"])
}
