fn main() -> Result<(), phoxal::build::Error> {
    phoxal::build::compile_protos(&["proto/phoxal/motion/v1/motion.proto"], &["proto"])
}
