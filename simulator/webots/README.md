# Webots Simulator Artifact

This directory is one official simulator artifact:
`simulator/webots/Cargo.toml` discovers as catalog id `simulator-webots` and
package `phoxal-simulator-webots`.

Webots normally runs external controller processes. The framework release
machinery packages one binary per artifact today, so this crate ships one binary
with a launch-time `mode` in `PHOXAL_CONFIG`:

- `bridge` (default) registers component devices and publishes simulator-owned
  `simulation/*` topics from one Supervisor-capable Webots controller process.
- `controller` registers component devices only.
- `supervisor` publishes simulator-owned clock/pose/contact only.

The emitted API metadata is the union of the contracts the artifact can satisfy,
which is what the simulator substitution checker needs. Runtime mode chooses
which Webots process role this particular launch performs.

The port intentionally covers the current framework slice only: motor command,
encoder, IMU, accelerometer, gyroscope, range, camera, depth, GNSS, and the
simulator clock/control/pose/contact contracts. Older Webots modules for LED,
microphone, lidar, battery, magnetometer, mmWave, and speaker are left as
explicit future work rather than partial ports.

The bridge keeps the old `webots-rs` linkage path. `webots-rs` builds with stub
bindings when Webots is absent so CI and catalog metadata generation stay green.
Running the native Webots backend requires a Webots-linked build on the host;
without that, startup fails with a diagnostic unless `require_native` is set to
`false` for pure contract tests.
