mod detector;
mod perception;
mod sensors;
mod tracker;

fn main() -> phoxal::Result<()> {
    phoxal::run::<perception::Perception>()
}
