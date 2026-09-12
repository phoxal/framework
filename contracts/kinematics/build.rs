fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(&["proto/phoxal/kinematics/v1/kinematics.proto"], &["proto"])
}
