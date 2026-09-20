//! Sandbox rover composition and mission policy.
mod config;
mod inputs;
mod outputs;
mod runtime;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(runtime::Brain)
}
