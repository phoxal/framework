//! A second binary of the same package: its private input must derive a
//! distinct identity from the driver binary's identically named module.

mod input;

fn main() {
    let _ = input::Reading::default();
}
