# phoxal-motion

Generated messages and typed public ports for the official final motion service.

The contract owns actuator targets, selected manual and autonomous motion intents, measured motion, protective safety state, authority status, and emergency/arm/disarm command responses.

Its generated ports are `ACTUATORS`, `MANUAL`, `AUTONOMOUS`, `STATUS`, and `EMERGENCY`.

The service runtime owns arbitration, expiry, emergency latching, and body-to-actuator conversion.

This contract package contains no motion algorithm, runtime, transport, driver, or simulator dependency.

The Motion implementation consumes Kinematics odometry directly, including its measured availability.
