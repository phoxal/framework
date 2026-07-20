# phoxal

A production-oriented framework for autonomous robots, shipped as one crate.

Phoxal gives a robot a small, strongly-typed core: a contract bus over Zenoh, version-qualified contracts, and a derive-based participant authoring model.
The framework owns the awkward parts - argument parsing, bus connection, scheduling, query serving, shutdown, and health - so the code you write is the robot's behavior, not its plumbing.

```toml
[dependencies]
phoxal = "0.32"      # the engine: runner, derives, SetupContext, prelude
phoxal-api = "0.20"  # the contract tree: `use phoxal_api::v1 as api;`
```

## The authoring model

A participant is a `Config` struct, an `Api` struct of typed bus handles, a state struct, and one annotated inherent impl.
The `Api` struct declares the contracts it uses (as handle fields); the impl declares the lifecycle.

```rust,ignore
use phoxal_api::v1 as api;
use phoxal::prelude::*;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {
    cruise_linear_x_mps: f32,
}

#[derive(phoxal::Api)]
struct Api {
    state:  Latest<api::drive::State>,
    target: Publisher<api::drive::Target>,
}

#[phoxal::service(id = "avoid-obstacles")]
struct AvoidObstacles {
    cruise_linear_x_mps: f32,
}

#[phoxal::behavior]
impl AvoidObstacles {
    #[setup]
    async fn setup(ctx: &mut SetupContext<Self>, config: Self::Config) -> Result<(Self, Self::Api)> {
        Ok((
            Self { cruise_linear_x_mps: config.cruise_linear_x_mps },
            Self::Api {
                state:  ctx.latest(api::topic::new().drive().state()).await?,
                target: ctx.publisher(api::topic::new().drive().target()).await?,
            },
        ))
    }

    #[step(hz = 50)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let now = step.time();
        // read inputs, publish version-local bodies
        api.target.publish_at(now, api::drive::Target {
            linear_x_mps: self.cruise_linear_x_mps,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        }).await?;
        Ok(())
    }
}

fn main() -> phoxal::Result<()> { phoxal::run::<AvoidObstacles>() }
```

Key rules the example shows:

- Import the stable or preview API modules you need, such as `use phoxal_api::v1 as api;` and `use phoxal_api::v2;`.
  An `Api` struct may mix contract bodies from different versions field by field, so there is no participant-wide API version to declare.
- Handle fields use version-qualified bodies: `Publisher<api::drive::Target>`, `Latest<api::drive::State>`.
  `#[derive(phoxal::Api)]` requires every field to name a `ContractBody`, so a non-contract type is a **compile error**.
- Topics are api-local: `api::topic::new().drive().state()`.
- The wire body is the plain payload, and contract identity lives only in the version-qualified topic key.
  Bus metadata carries the codec and provenance, including logical produce time and source identity, but no API version or contract family.
  Normal participants never open Zenoh - the runner opens the launch-selected bus profile before `#[setup]`.
- `config` is for **user participants only**.
  Official participants take no `config` param and read the robot model through `ctx.robot()`.

The runner also owns the rest of the lifecycle: `#[step(hz = ...)]` is the scheduled control loop, `#[server]` / `#[server_snapshot]` serve queries, and `#[shutdown]` runs graceful park/stop/flush before the bus closes.
It measures handler duration/lateness/missed ticks plus the exact
version-qualified typed-bus buffers declared by the participant and emits one
bounded portable runtime-performance rollup per host-monotonic grid interval.
The setup-declared rows persist for the process lifetime; interval counters are
best-effort boundary samples rather than a transactional queue snapshot. A
participant writes no telemetry code, unscheduled participants report step
timing as not applicable, and no per-process CPU/RSS sampler is involved.

`#[phoxal::tool]` is intentionally outside the robot clock. Tools use managed
host/event loops, host-monotonic timers for cadence and freshness, and
`phoxal::raw::host_time()` only when a generic bus envelope timestamp is
required. The macro rejects `#[step]`, the setup context exposes no clock, and
the tool process launch has no clock option. The normal embedding API likewise
accepts no clock argument; deterministic clock injection is restricted to typed
graph participants. Official tool sources are additionally checked against
importing or subscribing to simulation-clock surfaces; user-authored tools must
preserve the same boundary when using the privileged raw bus. A service that
consumes asynchronous tool input keeps a bounded latest value with
consumer-local monotonic arrival time and samples it at its own logical step.

## Modules

A participant depends on two crates: the `phoxal` engine (the modules below) and the `phoxal-api` contract tree.

| Module | What it is |
|---|---|
| `phoxal_api` (separate crate) | Current production `phoxal_api::v1` and evolving preview `phoxal_api::v2`, generated by `phoxal_api_tree!` from a tree of nested nodes: the marker `enum Api` (`ApiVersion`), version-local wire bodies + their `ContractBody` impls, and the api-local `topic` builders. Imported directly with `use phoxal_api::v1 as api;`; `v2` requires the `preview-v2` feature until promotion. |
| [`phoxal::prelude`] | Everything a participant author imports with `use phoxal::prelude::*;`: the handle types, `SetupContext`/`StepContext`, and `Result`. |
| [`phoxal::bus`] | Re-export of the `phoxal-bus` crate: the key scheme `<namespace>/robots/<robot-id>/<version-qualified-topic>`, the named-field MessagePack codec, provenance-only `BusMetadata`, and the body-typed handles `Publisher`/`Subscriber`/`Latest`/`Querier`. |
| [`phoxal::participant`] | The static metadata traits the macros target, `SetupContext`/`StepContext`/`ShutdownContext`, the clock (`RealClock`/`TestClock`) + scheduler, `ParticipantLaunch`, and the runner (`run` / `tokio::run`). |
| [`phoxal::model`] | Authored manifest schemas (`robot.yaml`, `structure.urdf`, `component.yaml`, …). |

## Authoring the API tree

The `phoxal_api` modules (in the `phoxal-api` crate) are generated by `phoxal_api_tree!` from a tree of **nested nodes**.
A node is `name { … }` (static) or `name(var) { … }` (dynamic, where `var` is a key segment filled at build time), nestable to any depth.
A node body holds any mix of `struct`/`enum` type declarations, `topic` declarations, and child nodes.
Each topic declares a **role**: `topic <leaf>: command <Body>;` (a control input the owning service subscribes), `topic <leaf>: state <Body>;` (telemetry the owning service publishes), or `topic <leaf>: query <Req> => <Resp>;` (request/response).
`command` and `state` are both pub/sub on the wire; the role selects the side brand of the generated builders (L1), so the public client builder (`api::topic::new()`) and the `internal` owner builder (`api::topic::internal::new(cap)`) return different branded topics (`Publish`/`Subscribe`) and taking the wrong side does not compile.
The owner builder additionally requires the runner-minted `OwnerCap` (L2): pass `ctx.owner_capability()`, so owning a topic is a deliberate, capability-gated opt-in rather than something that can happen by accident.
There are no key-template strings and no topic parameter lists; dynamism comes entirely from the `(var)` nodes on the path.

```rust,ignore
phoxal_api_tree! {
    version v1 {
        drive {                                  // static node
            struct Target { linear_x_mps: f32, angular_z_radps: f32 }
            topic target: command Target;        // key v1/drive/target
            struct State { /* … */ }
            topic state: state State;            // owner-published telemetry
        }
        component(instance) {                    // literal "component" + var {instance}
            motor(capability) {                  // literal "motor"     + var {capability}
                enum Command { Velocity(f32), Torque(f32), Stop }
                topic command: command Command;
            }
        }
    }
}
```

Everything a topic exposes is **derived from the node path** `n1 … nk` to its leaf (each `ni` has a `name` and an optional `var`):

- **Topic key** (`ContractBody::TOPIC`, version-qualified): the `vN` module is the first segment; each node then emits `name`, or `name/{var}` for a dynamic node, joined by `/`, followed by `/<leaf>`.
  So `drive` → `target` is `v1/drive/target`, and `component(instance)` → `motor(capability)` → `command` is `v1/component/{instance}/motor/{capability}/command`.
- **Module path / type location**: each node becomes a nested `pub mod name`, and a node's types + their `ContractBody` impls live in that module.
  Variables never appear in the module path, so the body above is `phoxal_api::v1::component::motor::Command`.
- **Contract identity** (`ContractBody::NAME`): the version-qualified Rust path, e.g. `v1::drive::Target` or `v1::component::motor::Command`.
- **Builder**: `api::topic::new().n1(var?)…nk(var?).leaf()`.
  A dynamic node's method takes its var as `impl Display` (`*` / `**` stay valid for subscribe); the leaf method yields the typed `Topic`.
  So `api::topic::new().component("front_left").motor("drive").command()` builds the key `v1/component/front_left/motor/drive/command`.

Each topic node is **self-contained**: it declares its own request, response,
and state bodies, with **no `super::`** and no shared/common module. This is
required because one body has one `ContractBody` identity and therefore one
topic. The narrow exception is a plain, non-topic protocol value declared by a
parent node and referenced from children through an absolute crate path. For
example, `v1::tool::Cursor` carries the same retention position in the separate
`tool::log` and `tool::bus` protocols, while each child still owns its distinct
`SnapshotRequest`, `Snapshot`, and `Follow` topic bodies.
Because the node path disambiguates, topic-body names are path-local -
`component::imu::Sample` and `component::range::Sample` are distinct types that
may safely repeat field names. Duplicating an identical topic body across
sibling nodes is intentional, not a smell.

During the current pre-stability simplification, coordinated production
corrections may change `v1` directly; the owner publishes before consumers
move. New preview work evolves in `v2`; there is no `extends` or copy-forward
tree.

## Entrypoints

- Default (blocking): `fn main() -> phoxal::Result<()> { phoxal::run::<R>() }`.
- Advanced (async): `phoxal::tokio::run::<R>().await` for custom Tokio mains.

## Official service set

The platform participant set ships alongside this crate in the workspace `service/` tree (`drive`, `motion`, `navigation`, `localize`, `map`, …).
They are full official participants authored on exactly this surface, and they double as worked reference reading.
Neutral teaching examples live in `phoxal/examples/` (`runtime_control_loop`, `runtime_query_server`, `runtime_snapshot_server`, `runtime_async_entrypoint`).

## Status

Pre-1.0, building in public.
There is no per-participant API version ceiling; the wire body is the plain payload and the version identity rides the version-qualified key.
Compatibility is checked at build/check time (compiled-in artifact metadata + `phoxal-cli check`), never by introspecting a running binary.

## License

AGPL-3.0-only.
A commercial license is available - see the repository.
