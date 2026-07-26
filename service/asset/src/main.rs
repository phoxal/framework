mod asset;

fn main() -> phoxal::Result<()> {
    phoxal::run::<asset::Asset>()
}
