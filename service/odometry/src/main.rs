mod odometry;

fn main() -> phoxal::Result<()> {
    phoxal::run::<odometry::Odometry>()
}
