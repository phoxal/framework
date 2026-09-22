// The binary target shares the package's contract library declared in the
// root `lib.rs`. The library stays contract-only; the binary brings in its
// own private modules here. Library types are accessed via the package name,
// `phoxal_service_kinematics`, which is the standard Cargo pattern for binaries
// in the same package as a non-default-location library.
mod config;
mod inputs;
mod outputs;
mod runtime;
mod validation;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(runtime::Kinematics)
}
