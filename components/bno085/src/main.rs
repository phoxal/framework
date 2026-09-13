mod config;
mod outputs;
mod runtime;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(runtime::Bno085)
}
