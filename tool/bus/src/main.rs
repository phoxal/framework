mod bus;

fn main() -> phoxal::Result<()> {
    phoxal::run::<bus::ToolBus>()
}
