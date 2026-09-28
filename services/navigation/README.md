# Navigation service

The Navigation executable owns bounded goal processing and publishes its state and terminal outcomes.
It consumes standard odometry and its own private map expectation through the robot's explicit graph connections.
Its Rust contract in `src/contract.rs` declares its endpoints, private `MapState` input, and specialized exported payloads.
The robot adapts World's `WorldRevision` to `MapState` in ordinary Rust conversion code.
Selecting the package in `robot.yaml` makes `api::navigation` available to that robot without adding a Navigation Cargo dependency.

Navigation does not claim a measured occupancy map or physical obstacle avoidance from generated types alone.
