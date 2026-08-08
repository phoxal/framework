mod follower;
mod frontiers;
mod navigation;
mod planner;

fn main() -> phoxal::Result<()> {
    phoxal::run::<navigation::Navigation>()
}
