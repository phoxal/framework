mod log;

fn main() -> phoxal::Result<()> {
    phoxal::run::<log::ToolLog>()
}
