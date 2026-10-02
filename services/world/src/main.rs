//! The world estimation service executable: configuration, the private
//! payload vocabulary and endpoint contract, validation, and the runtime.

mod config;
mod contract;
mod runtime;
mod validation;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<runtime::World>()
}
