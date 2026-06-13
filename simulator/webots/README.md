# Webots Simulator Integration

This directory contains the Webots local simulation workflow consumed by
[`phoxal-cli`](https://github.com/phoxal/phoxal-cli). `phoxal-cli simulate`
orchestrates the controller + supervisor binaries built from this workspace.

## Flow

From any robot project (a directory with a `robot.yaml`):

```bash
phoxal-cli simulate           # bring up router, runtimes, Webots
phoxal-cli simulate --dry-run # write .phoxal/run/ + compose without starting anything
```

`simulate` resolves `robot.yaml`, pulls platform runtime images from GHCR by
the locked digest, builds user runtimes locally, assembles `.phoxal/run/`
with the resolved robot view, generates docker-compose, and launches Webots
against the robot's `sim.world` path.

## Architecture

The simulation session is split into two Webots controller binaries:

- `phoxal-simulator-webots-supervisor`
- `phoxal-simulator-webots-controller`

### Supervisor responsibilities

- owns `webots.step(...)` pacing
- owns epoch generation (wall-clock-derived)
- publishes `simulation/clock`
- serves the `simulation/reset` command acknowledgement and applies the
  Webots reset

### Controller responsibilities

- subscribes to motor/led command topics
- applies commands to Webots devices
- owns its robot-local Webots `step(...)` calls; it does not subscribe to
  `simulation/clock`
- reads staged Webots PROTO metadata for controller contracts
- reads sensors and publishes stamped capability data topics

For camera-like capabilities:

- color and mono image streams are modeled with Webots `Camera`
- depth camera streams are modeled with Webots `RangeFinder`
- current depth output is the Webots-native simplified approximation used by
  the OAK-D Lite component plan, not controller-side stereo disparity
  reconstruction
- controller-published depth payloads quantize Webots meter samples into
  dense `u16` millimeter samples using the static component resolution; the
  controller skips publishing when Webots cannot provide a complete valid
  grid

## Robot identity in the staged world

The staged world the CLI generates contains two injected nodes per robot:

- a dedicated `Robot { supervisor TRUE }` node for the supervisor binary
- a generated robot PROTO node for the controller binary

The generated robot instance carries simulator-truth identity for scenario
runs:

- `DEF RF_ROBOT_<normalized_robot_id>`
- `name "<robot_id>"`

The normalized DEF suffix is uppercase ASCII with non-alphanumeric runs
collapsed to `_`; staging rejects duplicate normalized robot ids. The
supervisor resolves `simulation/robot/<robot_id>/pose` from this
convention without guessing from controller names.

The supervisor creates one `simulation/robot/<robot_id>/pose` publisher per
root robot node whose `DEF` starts with `RF_ROBOT_`. Every Webots step, it
re-resolves the node by `DEF`, reads the node's `translation` and
`rotation` fields, converts Webots axis-angle rotation to the API's
`rotation_xyzw`, and publishes a `world`-frame `Pose` stamped with the
supervisor's logical `time_ns` (the `at_ns` publish argument).

## Metadata source

Controller runtime metadata is encoded in staged PROTO comments (`# rf:`)
emitted by `phoxal-cli` during simulate staging. No separate metadata
artifact format is introduced.
