mod vl53l1x;

fn main() -> phoxal::Result<()> {
    phoxal::run::<vl53l1x::Vl53l1x>()
}
