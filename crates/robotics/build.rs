fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(&["proto/phoxal/robotics/v1/robotics.proto"], &["proto"])
}
