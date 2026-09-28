# Safety service

The Safety executable owns bounded assessment of measured range and world evidence and publishes expiring Motion constraints.
It does not own emergency authority, final actuation, hardware drivers, or a map algorithm.
Its Rust contract in `src/contract.rs` declares its endpoints and specialized payloads.
The robot selects its exact package version in `robot.yaml`; preparation extracts the compiled contract without a Safety library dependency.
