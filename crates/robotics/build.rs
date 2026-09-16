fn main() -> Result<(), phoxal::build::Error> {
    phoxal::build::compile_protos(&["proto/phoxal/robotics/v1/robotics.proto"], &["proto"])
}
