# Navigation service

The Navigation executable owns bounded goal processing and publishes its state and terminal outcomes.
It consumes selected Kinematics and World observations through the robot's explicit graph connections.
Its local `api/` tree contains the owned Navigation service and the message-only imports required for an independent package build.
The package's ordinary `build.rs` generates types with `phoxal::build::api`, and its binary attaches them with `phoxal::api!();`.
Selecting the package in `robot.yaml` makes `api::navigation` available to that robot without adding a Navigation Cargo dependency.

Navigation does not claim a measured occupancy map or physical obstacle avoidance from generated types alone.
