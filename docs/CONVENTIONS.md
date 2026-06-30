# Framework Conventions

Engineering conventions for the bus, topics, logical time, runtime authoring, and
components inside this workspace.
Rust style is in the org-level
[RUST_GUIDELINES](https://github.com/phoxal/organization/blob/master/docs/engineering/RUST_GUIDELINES.md);
contract discipline is in [CONTRACTS.md](./CONTRACTS.md).

## Bus and contracts

- Use `phoxal::bus` plus the `phoxal::api::<version>` modules for all inter-service
  communication.
  Do **not** add a direct `zenoh` dependency outside `phoxal::bus`.
- `phoxal::bus` owns the Zenoh session/builder (`Bus`), the body-typed handles
  (`Publisher`, `Subscriber`, `Latest`, `Querier`), the query/server primitives,
  and the `bus_abi` envelope: the topic-key scheme, the codec, the encoding string,
  and the `BusMetadata` attachment.
  Runtime, driver, and tool code connects through the runner-owned bus (it does not
  open Zenoh itself) and names topics through the api modules.
- The wire body is the **plain MessagePack payload** of a version-local body type;
  `api_version`/`family`/`codec` and the produce-time stamp ride bus metadata, never
  the body or the key (see [CONTRACTS.md](./CONTRACTS.md)).
- Endpoints use Zenoh endpoint syntax directly (`tcp/127.0.0.1:7447`,
  `tcp/router:7447`); endpoint literals and device paths live in the
  manifest/bundle/launch layer, never in runtime source.
- By convention each topic has one producer: products (state, telemetry) are read
  via `Subscriber`/`Latest` and commands are sent via `Publisher`, with the opposite
  side owned by whoever produces the effect.
  Avoid `pub use` re-export shims - name contracts through the `api` alias.

## Topic naming

Topic keys are **versionless** and api-local; the api tree's `topic` builders are
the only source of keys, and the wire body never appears in the key
([`phoxal-api/src/lib.rs`](../phoxal-api/src/lib.rs),
[`phoxal-bus/src/topic.rs`](../phoxal-bus/src/topic.rs)).

- Domain streams: `<domain>/<stream>` (e.g. `drive/state`, `drive/target`,
  `safety/authorization`, `mission/state`), built as `api::topic::new().drive().state()`.
- Domain queries: a single `<domain>/<query>` key carrying request + response
  bodies (e.g. `frame/lookup`, `map/submap`, `asset/get`).
- Per-instance component capabilities:
  `component/<instance>/<kind>/<capability>/<stream>` (e.g.
  `component/front_left_drive/motor/motor/command`), built as
  `api::topic::new().component(instance).motor(capability).command()`.
  These are dynamic keys resolved from the robot model in `#[setup]`.
- The runner applies the multi-robot root `<namespace>/robots/<robot-id>/` to every
  key at the transport layer; runtime code only ever names the versionless key.
- `publish_key()` rejects a wildcard key before transport; wildcard subscription
  stays allowed for discovery/driver use.

## Pub/sub and telemetry

- A pub/sub body is a plain serde struct/enum.
  Produce time is **not** a body field; it rides `BusMetadata` and is stamped from
  the runtime's `LogicalTime` via `publisher.publish_at(at, body)`.
- A publish never blocks the step loop: `publish_at` is a non-blocking enqueue onto
  a bounded outbound queue.
  A saturated queue drops the sample, bumps `outbound_drops`, and returns
  `BusError::Saturated` so the loss is observable - there is no reliable/blocking
  publish variant ([`phoxal-bus/src/handle.rs`](../phoxal-bus/src/handle.rs)).
- Receivers bound their backlog: `Latest<B>` keeps last-1; `Subscriber<B>` is a
  drop-oldest ring (depth 32 by default, overridable), bumping `inbound_drops` when
  a slow consumer lets the ring fill.
- When a runtime needs to explain a decision, prefer a typed reason field on the
  primary contract (a closed-set enum + optional `detail`); only promote it to its
  own topic if another runtime branches on it.

## Logical time

- Runtimes and drivers track time, watchdogs, and staleness with **logical time**
  only - never wall time directly.
  The runner owns one `ClockSource` and stamps every `StepContext` and every
  `produced_at_ns` from it, so all participants share one time domain
  ([`phoxal/src/runtime/clock.rs`](../phoxal/src/runtime/clock.rs)).
- `LogicalTime` is `{ epoch, time_ns }`.
  Within an epoch `time_ns` strictly increases; an epoch bump signals a reset.
  `RealClock` reads the host-wide UNIX-epoch domain (so cross-process staleness
  checks are comparable) and latches monotonically; `TestClock` is an injectable
  fake for tests.
- The simulation clock source (subscribing the supervisor's authoritative
  `simulation/clock`) is **not yet implemented**: requesting `ClockMode::Simulation`
  is rejected today and lands with the Webots port.
  Next-step (`S` -> `S+1`) simulation visibility is part of that future work, not a
  current behavior.

## Runtime authoring

- A runtime is a struct with `#[derive(phoxal::Runtime)]` plus a bare
  `#[phoxal::runtime]` on its inherent impl
  ([`phoxal/src/lib.rs`](../phoxal/src/lib.rs),
  [`runtime/drive/src/main.rs`](../runtime/drive/src/main.rs)).
  The derive carries config (`#[phoxal(id = "…", api = y2026_1)]`, optional
  `config = path::Config`) and reads the typed handle fields; the attribute owns the
  lifecycle/server methods.
  There is no visible `Runtime` trait and no `execute(...)` entrypoint.
- Lifecycle methods: `#[setup]` (mandatory, builds all IO from
  `SetupContext<Self>`), `#[step(hz = N)]` (scheduled control step, at most one),
  `#[shutdown]` (graceful park/flush before bus close, at most one).
  Query servers: `#[server(topic = …)]` (exclusive, holds `&mut self`, serialized
  with `#[step]`), `#[server_snapshot(topic = …)]` (concurrent, reads a committed
  `Snapshot`), `#[snapshot]` (the committed-snapshot provider).
- Handles are typed fields: `Publisher<B>`, `Subscriber<B>`, `Latest<B>`,
  `Querier<Req, Resp>`, or a `Vec`/`BTreeMap` of one for per-instance IO.
  Every body must satisfy `ContractBody<Api = R::Api>` and the runtime must have
  declared the family - both are compile-enforced, so a wrong-version body or an
  undeclared family is a compile error
  ([`phoxal/src/runtime/context.rs`](../phoxal/src/runtime/context.rs)).
- The entrypoint is plain Rust: `fn main() -> phoxal::Result<()> {
  phoxal::run::<Runtime>() }`.
  The runner owns args/env, bundle + robot-model loading, the bus connection, the
  clock, step scheduling, server dispatch, snapshot commits, and shutdown.
  Every binary also answers a top-level `emit-apis` subcommand that prints its
  static metadata as one JSON document and exits before any of that
  ([`phoxal/src/runtime/emit.rs`](../phoxal/src/runtime/emit.rs)).
- Official runtimes take no typed config and build their state from the robot model
  via `ctx.robot()` (`Config::from_robot(&Robot)` in the runtime's own code - there
  is no shared resolver abstraction).
  User runtimes declare `config = path::Config` and read it as `Self::Config` in
  `#[setup]`.

## Components

- Component capability contracts live under the api tree's `component(instance)`
  node ([`phoxal-api/src/lib.rs`](../phoxal-api/src/lib.rs)); each `kind(capability)`
  child is a self-contained node whose key is
  `component/<instance>/<kind>/<capability>/<leaf>`.
- A component driver is an ordinary runtime launched once per `components.instances`
  entry; the bound instance is provided via `ctx.component()`, and the driver derives
  its per-instance handles from the robot model.
- `component.yaml` is the only source of component-local capability definitions;
  `robot.yaml` does not override component-local fields.
  `robot.yaml` component entries are instance-only (type, mount link, per-instance
  `driver` config with an explicit `connection`, sparse per-capability `parameters`).
- Keep component-local ids local in the component source; namespace them only at
  robot composition boundaries (`<instance-id>.<capability-id>`, the `CapabilityRef`).
  The `CapabilityRef` is distinct from the bus key - the manifest binding maps a
  `CapabilityRef` to the concrete `component/…` key.
