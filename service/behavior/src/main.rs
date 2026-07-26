mod behavior;

fn main() -> phoxal::Result<()> {
    phoxal::run::<behavior::BehaviorService>()
}
