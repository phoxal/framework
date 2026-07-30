mod behavior;
mod catalog;

fn main() -> phoxal::Result<()> {
    phoxal::run::<behavior::BehaviorService>()
}
