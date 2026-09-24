# World service

The World executable owns bounded localization belief, coherent revisions, immutable map-window reads, and explicit unavailable results.
Its current free-space fixture does not claim SLAM, perception, map persistence, or mature spatial planning.
Its package contains one owned World service under `api/`, an ordinary `build.rs`, and a runnable binary.
The robot selects its exact package version in `robot.yaml`; the package's Protobuf closure is prepared for the robot build script without a World library dependency.
