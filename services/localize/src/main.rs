mod localize;

fn main() -> phoxal::Result<()> {
    phoxal::run::<localize::Localize>()
}
