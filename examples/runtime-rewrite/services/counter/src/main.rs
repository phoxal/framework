fn main() -> phoxal::Result<()> {
    phoxal::runtime::run(example_counter_service::Counter)
}
