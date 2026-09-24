# Safety service

The Safety executable owns bounded assessment of measured range and world evidence and publishes expiring Motion constraints.
It does not own emergency authority, final actuation, hardware drivers, or a map algorithm.
Its package contains one owned Safety service under `api/`, an ordinary `build.rs`, and a runnable binary.
The robot selects its exact package version in `robot.yaml`; the package's Protobuf closure is prepared for the robot build script without a Safety library dependency.
