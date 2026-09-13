# Motion service

This executable owns final actuator intent, arm/disarm state, and emergency handling.
Its library exports only the messages and ports owned by [contract](contract/README.md).
Configuration, inputs, outputs, arbitration, and drive calculations live in separate executable modules.

Configure `left_wheels` and `right_wheels` with one to four entries per side.
Each entry names an `actuator_id`, its motor-to-wheel `gear_ratio`, and its `direction_sign`.
The shared `wheel_radius_m` and `wheel_base_m` are metres; the latter is the distance between the left and right contact lines.
Every output contains the complete configured actuator membership, including stopped commands.

The runtime starts disarmed and requires an explicit arm command, matching intent ownership, valid protective constraints, and available measured odometry.
Both derived inputs must retain fresh original capture evidence; a new publication timestamp cannot make stale sensor data usable.
Motor shaft rates apply each wheel's gearing and direction exactly once after converting the bounded body twist to wheel velocity.
