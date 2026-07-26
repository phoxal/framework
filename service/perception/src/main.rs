mod detector;
mod frames;
mod perception;
mod sensors;
mod tracker;

fn main() -> phoxal::Result<()> {
    phoxal::run::<perception::Perception>()
}
