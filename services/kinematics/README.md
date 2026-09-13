# Kinematics service

This executable estimates planar differential or skid-steer motion from every configured wheel encoder.
Its library exports only the messages and ports owned by [contract](contract/README.md).
Configuration, inputs, outputs, runtime state, and measurement calculations live in separate executable modules.

Configure `left_wheels` and `right_wheels` with one to four entries per side.
Each entry names its `encoder_id`, `joint_id`, `direction_sign`, `gear_ratio`, and optional `longitudinal_offset_m` from the body origin.
The shared `wheel_radius_m` and `wheel_base_m` are metres; the latter is the distance between the left and right contact lines.
The planar frame tree describes wheel centers, not three-dimensional wheel orientation.

Each encoder shaft velocity is divided by its gearing, multiplied by its direction and wheel radius, and averaged with the other wheels on that side.
The runtime integrates the resulting body twist over the logical interval using the constant-turn solution.
It does not infer slip or promise calibrated physical odometry.

Required captures may be retained between source publications up to `max_age_ms`.
The oldest source capture remains explicit in odometry and is never renewed by retention or republication.
Missing, invalid, or stale required wheel evidence makes odometry unavailable.
Only newly captured joint samples are published as Samples.
Frame history has an explicit maximum of 256 entries.
