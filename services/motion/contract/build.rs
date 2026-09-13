fn main() -> Result<(), phoxal_build::Error> {
    phoxal_build::compile_protos(&["proto/phoxal/motion/v1/motion.proto"], &["proto"])
}
