mod capabilities;
mod webots_controller;

fn main() -> phoxal::Result<()> {
    webots_controller::run()
}
