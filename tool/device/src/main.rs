mod device;

fn main() -> phoxal::Result<()> {
    phoxal::run::<device::ToolDevice>()
}
