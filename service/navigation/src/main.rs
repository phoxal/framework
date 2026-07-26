mod exploration;
mod follower;
mod frontiers;
mod navigation;
mod planner;
mod scoring;

fn main() -> phoxal::Result<()> {
    phoxal::run::<navigation::Navigation>()
}
