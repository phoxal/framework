mod safety;

fn main() -> phoxal::Result<()> {
    phoxal::run::<safety::Safety>()
}
