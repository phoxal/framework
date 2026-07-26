mod telemetry;

fn main() -> phoxal::Result<()> {
    phoxal::run::<telemetry::ToolTelemetry>()
}
