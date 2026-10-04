//! Captures build-time facts the compiled SDK needs.
//!
//! The concrete target triple records what this build actually targets:
//! resolved bundles name their deployment target explicitly, and consumers
//! compare it with their own build-time target rather than a reconstruction
//! from cfg fragments.

fn main() {
    println!(
        "cargo:rustc-env=PHOXAL_API_GENERATOR_VERSION={}",
        phoxal_build::GENERATOR_VERSION
    );
    let target = std::env::var("TARGET").unwrap_or_else(|error| {
        eprintln!("Cargo did not provide TARGET to the phoxal build script: {error}");
        std::process::exit(1);
    });
    println!("cargo:rustc-env=PHOXAL_HOST_TARGET={target}");
}
