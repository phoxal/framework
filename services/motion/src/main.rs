mod arbitration;
mod motion;

fn main() -> phoxal::Result<()> {
    phoxal::run::<motion::Motion>()
}
