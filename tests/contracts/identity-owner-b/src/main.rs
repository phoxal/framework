//! A private input whose identity must be qualified by the owning
//! package even when the binary target name matches another package's.

mod input;

fn main() {
    let _ = input::Reading::default();
}
