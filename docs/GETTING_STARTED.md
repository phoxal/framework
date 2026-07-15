# Getting Started: Hand-Authoring a Robot Project

A Phoxal robot project is a directory you write by hand: a `robot.yaml`
manifest, a `structure.urdf`, zero or more component definitions, and zero or
more user service crates.
There is no `scaffold`, `create`, or `pull` command that generates one for
you.
This guide walks through the pieces using the checked-in
[`examples/hello-rover`](../examples/hello-rover) project, a minimal
differential-drive rover with one component type and one user service, and
points at the JSON Schema you can wire into your editor for inline validation
as you write.

Everything here assumes `phoxal-cli` is on your `PATH` (see the
[phoxal-cli README](https://github.com/phoxal/phoxal-cli#readme) for install
instructions) and that you have a working Rust toolchain for the user service
crate.

## The example project

```text
examples/hello-rover/
  robot.yaml                       # the manifest
  robot.dev.yaml                   # dev-only overlay: local path pins
  structure.urdf                   # the robot's own URDF (chassis + mounts)
  components/wheel_drive/          # one robot-local component definition
    component.yaml
    structure.urdf
```

`hello-rover` is a two-wheel differential-drive rover.
Both wheels are instances of the same `wheel_drive` component type. Motion is
commanded through the official manual or navigation candidate contracts; the
example intentionally carries no always-moving cruise service.
It is deliberately small: no sensors beyond what the kinematic model needs,
no simulation world, and no hardware driver.
Use it as the shape to copy from, not as a real robot.

## `robot.yaml` anatomy

```yaml
schema: robot/v0
robot:
  id: hello-rover
  namespace: dev
  structure: structure.urdf
  kinematic:
    kind: differential
    left_actuators: [left_drive.motor]
    right_actuators: [right_drive.motor]
    left_encoders: [left_drive.encoder]
    right_encoders: [right_drive.encoder]
    wheel_radius_m: 0.05
    wheel_base_m: 0.3
  components:
    left_drive:
      component: wheel_drive
      mount_link: left_wheel_mount
    right_drive:
      component: wheel_drive
      mount_link: right_wheel_mount
services:
  cruise:
    path: services/cruise
    config:
      cruise_speed_mps: 0.2
```

The root keys are `schema`, `robot`, and optionally `artifacts`, `services`,
and `bus`:

- `schema` is always `robot/v0` today.
  It is the only version discriminator; there is no separate `version:` key.
- `robot` is the robot model: `id`, `namespace`, the URDF `structure` path,
  the `kinematic` model, and the `components` instance map.
  This is everything `ctx.robot()` exposes to a running participant.
- `kinematic` is a direct field of `robot:`, not nested under a `motion:`
  wrapper.
  Its `kind` selects `differential`, `mecanum`, `ackermann`, or
  `omnidirectional`, each with its own actuator/encoder fields.
  An actuator or encoder reference is a plain `"<component-instance>.<capability>"`
  string, e.g. `left_drive.motor` points at the `motor` capability of the
  `left_drive` component instance.
- `components` is a flat instance map keyed by instance id.
  Each instance names a `component` type and a `mount_link` (a link name from
  `structure.urdf`), and optionally a `driver` (a real hardware connection)
  and `parameters` (per-capability tuning like `direction_sign`).
  There is no `components.sources`/`components.instances` split and no
  `identity:` wrapper: those are retired grammar.
  An official component (like `ddsm115` or `bno085` in this repo's own
  `component/`) resolves automatically from the artifact catalog by its
  logical id; a robot-local component like `hello-rover`'s own `wheel_drive`
  does not need a `driver:` block at all if it has no hardware driver
  participant (see [Adding a component](#adding-a-component)).
- `artifacts` (optional) controls the release `channel` and any
  `artifacts.pins` overrides.
  Base `robot.yaml` is fail-closed for `{ path: ... }` pins; those are legal
  only in a `robot.<env>.yaml` overlay, loaded with `phoxal-cli check --env
  <env>`.
  `hello-rover`'s `robot.dev.yaml` is exactly that overlay.
- `services` (optional) declares user services only.
  Official services are never declared here; they resolve from the artifact
  catalog automatically.
- `bus` (optional) configures router listen endpoints and an upstream uplink.
  `hello-rover` needs neither and omits the section entirely.

## Adding a component

A component is a directory with its own `component.yaml` (schema
`component/v0`) declaring its capabilities, and its own `structure.urdf`
fragment describing the physical parts a driver or simulator actuates.
`hello-rover`'s `components/wheel_drive/component.yaml`:

```yaml
schema: component/v0
capabilities:
  motor:
    kind: motor
    command: velocity
    max_torque_nm: 1.0
    max_velocity_radps: 20.0
    gear_ratio: 1.0
    target:
      kind: joint
      id: motor_joint
  encoder:
    kind: encoder
    publish_rate_hz: 50.0
    gear_ratio: 1.0
    encoder_type: incremental
    counts_per_revolution: 2048
    target:
      kind: joint
      id: motor_joint
```

Each capability's `target` names a joint or link in the component's own
`structure.urdf` (here, `motor_joint`); the robot's own `robot.yaml` then
points a kinematic actuator/encoder at `<instance>.<capability>`
(`left_drive.motor`, `left_drive.encoder`).

A component instance in `robot.yaml` needs a `driver:` block only if a real
hardware driver participant runs for it.
`hello-rover`'s wheels have none (there is no `phoxal::driver` binary in this
example), so `left_drive`/`right_drive` list only `component` and
`mount_link` - the same shape robot-v1's driverless `passive_caster` uses.
A robot-local component is not in the official artifact catalog, so pin its
assets from an overlay:

```yaml
# robot.dev.yaml
artifacts:
  pins:
    phoxal/component-wheel_drive:
      path: ./components/wheel_drive
```

Load the overlay with `phoxal-cli check --env dev` (or `simulate ... --env
dev`) whenever you need the local pin; a driverless component that has no pin
at all also resolves fine (`check` treats a missing catalog entry for a
driverless component as valid, not an error) but the pin is what lets
`simulate` find the local mesh/URDF assets.

## Writing a user service

A user service is an ordinary Cargo binary crate that depends on the
workspace's `phoxal`/`phoxal-api` crates, plus:

- one `#[derive(phoxal::Config)]` struct read from `robot.yaml`'s
  `services.<name>.config` (skip it if the service takes no config, matching
  the `config = ()` official services use);
- one `#[derive(phoxal::Api)]` struct of typed bus handles
  (`Publisher<B>`/`Subscriber<B>`/`Latest<B>`/`Server<Req, Resp>`), one field
  per contract the service needs;
- a `#[phoxal::service(id = "...")]` struct for its own runtime state, plus a
  `#[phoxal::behavior]` block with `#[setup]` (builds the `Api` from
  `SetupContext`) and `#[step(hz = N)]` (the control loop).

The smallest motion-producing user service publishes a manual candidate; the
official `motion` service owns arbitration, freshness, limits, and e-stop:

```rust
use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::v1 as api;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {
    cruise_speed_mps: f32,
}

#[derive(phoxal::Api)]
struct Api { manual: Publisher<api::motion::ManualCommand> }

#[phoxal::service(id = "cruise")]
struct Cruise {
    cruise_speed_mps: f32,
}

#[phoxal::behavior]
impl Cruise {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        config: Self::Config,
    ) -> Result<(Self, Self::Api)> {
        Ok((
            Self { cruise_speed_mps: config.cruise_speed_mps },
            Self::Api {
                manual: ctx.publisher(api::topic::new().motion().manual()).await?,
            },
        ))
    }

    #[step(hz = 10)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        api.manual.publish_at(step.time(), api::motion::ManualCommand {
            linear_x_mps: f64::from(self.cruise_speed_mps),
            angular_z_radps: 0.0,
        }).await?;
        Ok(())
    }
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Cruise>()
}
```

A few points that generalize beyond this one service:

- A user service authors against **official contracts** from
  `phoxal_api::v1` (or whichever API version you target).
  It never mints its own bus types; contracts are the shared vocabulary
  every participant on the graph already speaks.
- `robot.yaml` names the service by its `services.<name>` key and points
  `path` at the crate directory (relative to `robot.yaml`); the config block
  under it is validated against the crate's own compile-time JSON Schema
  (from `#[derive(phoxal::Config)]`) by `phoxal-cli check`.
- The crate needs its own `Cargo.toml` with a `phoxal` dependency; outside
  this repo, pin it to a released version (`phoxal = "0.32"`, or a git tag).
  `hello-rover`'s own `services/cruise/Cargo.toml` uses
  `phoxal = { workspace = true }` instead, because it is a workspace member
  of the `phoxal/framework` repo itself and always builds against the
  in-tree source, not a release.
  `phoxal-cli validate`'s dependency-pin check does not recognize a
  workspace path dependency as a valid pin, so validating this specific
  example needs `--allow-user-service-drift` (a real out-of-tree project
  should never need that flag - see the [phoxal-cli
  README](https://github.com/phoxal/phoxal-cli#readme)).

## Editor support: the `robot.yaml` JSON Schema

[`examples/robot.schema.json`](../examples/robot.schema.json) is a JSON
Schema (Draft 2020-12) for the `robot/v0` grammar, generated directly from
the same Rust model `phoxal-cli` parses `robot.yaml` against
(`phoxal::model::robot::Robot` and everything it reaches).
It is not hand-maintained: a `cargo test -p phoxal` guard
(`model::robot::schema_guard`) fails the build the moment the checked-in file
stops matching what the model actually accepts, so it cannot silently rot.

Point your editor's YAML language server at it, either via a per-file
directive at the top of `robot.yaml`:

```yaml
# yaml-language-server: $schema=../framework/examples/robot.schema.json
schema: robot/v0
robot:
  ...
```

or via a `yaml.schemas` entry in your editor settings mapping
`robot.schema.json` to `robot.yaml`/`robot.*.yaml`.
Either way you get inline validation and completion for the manifest grammar
as you type, including the closed set of kinematic kinds, connection types,
and capability kinds.

## Running `check` and `simulate`

From the project directory:

```sh
phoxal-cli check              # resolve the graph, validate it, no --env needed
phoxal-cli check --env dev    # same, with robot.dev.yaml's local component pin applied
phoxal-cli validate --report --allow-user-service-drift   # lower-level structural check (see above)
```

`check` resolves every official participant (services, tools, and any
catalog-sourced component drivers) plus every user service and prints
`ok: N participants validated` once the whole graph is coherent.
`hello-rover` has no Webots world checked in (its component uses primitive
URDF geometry, not meshes, so there is nothing to render yet); a project that
wants `phoxal-cli simulate <world>` needs a `worlds/<world>.wbt` file, the
same way [`robot-v1`](https://github.com/phoxal/robot-v1) does.

## Where to go from here

- [`docs/CONVENTIONS.md`](CONVENTIONS.md) - bus/contract/participant-authoring
  conventions in full, including the four authoring kinds
  (`service`/`driver`/`tool`/`simulator`) and the component capability model.
- [`docs/CONTRACTS.md`](CONTRACTS.md) - contract discipline: naming,
  versioning, and compatibility rules for the `phoxal-api` tree.
- [robot-v1](https://github.com/phoxal/robot-v1) - a fuller reference robot
  with sensors, a simulation world, and a dev overlay pinning every official
  service to source for local development.
