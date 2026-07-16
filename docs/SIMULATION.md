# Simulation

`phoxal-cli simulation run <world>` runs a robot's graph host-natively against
a live Webots session.
Everything is a native process; there is no container runtime.

## How it works

- The CLI resolves the robot graph, checks it, and stages a Webots world under
  `<project>/.phoxal/webots/`.
  It copies the authored `<project>/worlds/<world>.wbt`, injects the generated
  robot PROTO, and declares that PROTO `IMPORTABLE EXTERNPROTO` so the supervisor
  can instantiate it at runtime.
- The CLI launches Webots as its only simulator-side child, pointed at the staged
  world, in `--mode=realtime --batch`.
  Webots opens a world paused by default, so the explicit run mode is required for
  the simulation to advance; `--batch` suppresses blocking dialogs so a supervised
  shutdown is clean.
- Webots starts the `phoxal-simulator-webots-supervisor` controller.
  The supervisor is the world/session authority: it imports each robot node
  (`importMFNodeFromString`), owns the Webots Supervisor API, and publishes the
  authoritative `simulation/clock`, `robot_pose`, and `contact` feeds.
- Each imported robot node starts a `phoxal-simulator-webots-controller`
  controller that binds the robot's devices, publishes component contracts
  (encoder, imu, camera, range, gnss, ...), and applies actuator commands.
- The user service graph runs as ordinary bus participants and is driven by the
  supervisor's `simulation/clock`.

### Clock domain

The two Webots-linked simulators (supervisor and controllers) are the Webots-side
clock authority. Their launch contract is structurally clockless: simulator
binaries expose neither `--clock` nor `PHOXAL_CLOCK`, and callers cannot select
their runner clock. They self-drive through `wb_robot_step` (both spawn
`synchronization TRUE`, so Webots does not advance until each has stepped) on the
fixed host scheduler.
The supervisor derives logical simulation time from Webots and publishes it on
the `v2/simulation/clock` contract after each completed world step. The
payload is `{ now_ns, step }`: publication itself is the advancement signal,
and silence means Webots has not advanced. There is no separate pause flag.

Clock-selectable robot participants (services, and in a live robot the component
drivers) run on `ClockMode::Simulation`: their `#[step]` is released by the
`simulation/clock` feed and its `produced_at_ns` is stamped from the same
simulation-time source, so cross-participant staleness checks
compare timestamps in one domain.
A pure-bus participant seeds its simulation scheduler at logical zero, because the
feed publishes 0-based simulation time and logical time only advances forward.

## Acceptance record - single robot (2026-07-12)

Command: `phoxal-cli simulation run default --env dev` in the `robot-v1` reference
project, host-native against Webots R2025a on macOS.

Observed over a bus probe of the live session:

- Webots opened the staged world and imported the `RobotV1` PROTO at runtime; the
  per-robot controller process started (robot present).
- Exactly one supervisor (`simulator-webots-supervisor`) and one controller
  (`simulator-webots-controller-robot-v1`) were present.
- `simulation/clock` advanced in simulation time and every clock-follower stepped
  in that domain.
- Component contracts flowed from the Webots controller through consuming
  participants, e.g. `component/left_drive/encoder` -> odometry ->
  `odometry/state`; the full sensor set (imu, encoder, camera rgb/depth/mono,
  every ToF range, gnss) published.
- The complete autonomy graph produced derived state with no unhealthy
  participants: asset, drive, frame, joint, localize, map, motion, navigation,
  odometry, perception, power, presence, and video. Battery telemetry is owned
  by the simulator controller rather than a platform service.
- SIGINT shut the session down cleanly with no orphaned processes.

Observed-readiness and failure propagation (added after the initial record, now
part of the tested path):

- Readiness is OBSERVED, not assumed: every participant reaches `Ready` only when
  its own `presence/heartbeat` is seen going Ready. Startup requires all expected
  participants Ready; clock telemetry is observational and never gates session
  readiness. All 39 participants reached Ready this way.
- Simulation-managed failure is detected and propagated: killing the Webots
  controller mid-run marked it `Failed` within ~6s (heartbeat staleness), the
  session tore down automatically, and `simulation run` exited non-zero with
  `graph ended unhealthy; failed participants: …` - no operator intervention. A
  crashed-then-restarted service that recovers does not trip this.

The same single-robot graph also runs on the plain
`phoxal-cli simulation run default` path (vendored artifacts, no `--env dev`):
the Webots simulator binaries are built against Webots in release CI, so the
downloaded controllers are runtime-linked; 39/39 reached Ready in ~12s with a
clean shutdown.

The framework participant suite and the CLI suite were green at the time of this
record.

## Multi-robot status - deferred, needs a product decision

The framework-rewrite Gate 1 acceptance list included a two-robot smoke (two
isolated controllers, one supervisor, one clock authority, no participant or topic
collisions).
That criterion is **not met and is deliberately deferred**, recorded here rather
than silently dropped:

- The public `simulate` path stages a single robot: it builds the launch plan and
  Webots staging from one robot slice, and the supervisor's launch record is
  scoped to that robot's id and namespace.
- The clock-authority topology across multiple robot-scoped bus roots is
  unresolved: a supervisor connected as one robot cannot publish the authoritative
  clock into a second robot's bus root.
  A world-scoped clock/session authority that spans multiple robot buses is the
  design question to settle before multi-robot simulation is implemented.

Single-robot simulation is fully supported and is the tested path.
Multi-robot simulation requires the topology decision above plus staging and
launch-plan support for N robots; it is tracked as follow-up work, not as shipped
behavior.
