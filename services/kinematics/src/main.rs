mod config;
mod contract;
mod runtime;
mod validation;

fn main() -> phoxal::Result<()> {
    phoxal::runtime::run::<runtime::Kinematics>()
}
