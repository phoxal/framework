# Framework Conventions

Engineering conventions for the bus, topics, logical time, participant authoring, and
components inside this workspace.
Rust style is in the org-level
[RUST_GUIDELINES](https://github.com/phoxal/organization/blob/master/docs/engineering/RUST_GUIDELINES.md);
contract discipline is in [CONTRACTS.md](./CONTRACTS.md).

## Bus and contracts

- Use `phoxal::bus` plus the `phoxal_api::<version>` modules for all inter-service
  communication.
  Do **not** add a direct `zenoh` dependency outside `phoxal::bus`.
- `phoxal::bus` owns the Zenoh session/builder (`Bus`), the body-typed handles
  (`Publisher`, `Subscriber`, `Latest`, `Querier`), the query/server primitives,
  and the wire ABI: the topic-key scheme, the codec, the encoding string, and the
  `BusMetadata` attachment.
  Service, driver, tool, and simulator code connect through the runner-owned bus
  (they do not open Zenoh themselves) and name topics through the api modules.
- The wire body is the **plain MessagePack payload** of a version-local body type.
  Compatibility keys on exact name identity (D1): the generation is folded into
  the Zenoh key itself, so two participants interoperate on a contract iff they
  use the exact same version-qualified name. There is no `schema_id`/`family`
  hash. Only the codec and the produce-time stamp ride bus metadata, never the
  body or the key (see [CONTRACTS.md](./CONTRACTS.md)).
- Endpoints use Zenoh endpoint syntax directly (`tcp/127.0.0.1:7447`,
  `tcp/router:7447`); endpoint literals and device paths live in the
  manifest/launch layer, never in service source.
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
  key at the transport layer; service code only ever names the versionless key.
- `publish_key()` rejects a wildcard key before transport; wildcard subscription
  stays allowed for discovery/driver use.

## Pub/sub and telemetry

- A pub/sub body is a plain serde struct/enum.
  Produce time is **not** a body field; it rides `BusMetadata` and is stamped from
  the participant's `LogicalTime` via `publisher.publish_at(at, body)`.
- A publish never blocks the step loop: `publish_at` is a non-blocking enqueue onto
  a bounded outbound queue.
  A saturated queue drops the sample, bumps `outbound_drops`, and returns
  `BusError::Saturated` so the loss is observable - there is no reliable/blocking
  publish variant ([`phoxal-bus/src/handle.rs`](../phoxal-bus/src/handle.rs)).
- Receivers bound their backlog: `Latest<B>` keeps last-1; `Subscriber<B>` is a
  drop-oldest ring (depth 32 by default, overridable), bumping `inbound_drops` when
  a slow consumer lets the ring fill.
- When a service needs to explain a decision, prefer a typed reason field on the
  primary contract (a closed-set enum + optional `detail`); only promote it to its
  own topic if another service branches on it.

## Logical time

- Participants track time, watchdogs, and staleness with **logical time**
  only - never wall time directly.
  The runner owns one `ClockSource` and stamps every `StepContext` and every
  `produced_at_ns` from it, so all participants share one time domain
  ([`phoxal/src/participant/clock.rs`](../phoxal/src/participant/clock.rs)).
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

## Participant authoring

- A participant is two cooperating structs plus a bare `#[phoxal::behavior]` on
  the inherent impl
  ([`phoxal/src/lib.rs`](../phoxal/src/lib.rs),
  [`service/drive/src/main.rs`](../service/drive/src/main.rs)):
  a state struct linked with one authoring attribute
  (`#[phoxal::service(id = "…", config = …)]`), and a companion handle struct
  with `#[derive(phoxal::Api)]` whose typed fields declare the bus-facing
  contract surface.
  A `config = path::Config` struct derives `#[derive(phoxal::Config)]`.
  There is no visible umbrella trait and no `execute(...)` entrypoint.
- The four authoring kinds are attribute macros on the state struct:
  `#[phoxal::service]` for ordinary typed participants;
  `#[phoxal::driver]` for per-component-instance participants that can call
  `ctx.component()`;
  `#[phoxal::tool]` for host-side utilities that inspect `ctx.robot()` (`Api`
  defaults to `()` - tools stay raw-bus only);
  `#[phoxal::simulator]` for simulation-only participants.
  Each kind embeds its own JSON metadata (id, kind, contract surface) as a
  static in a dedicated linker section on the compiled binary
  (`.phoxal_api_meta` / `__phoxal_meta`), so a consumer reads it straight out of
  the object file without ever executing the artifact.
- Lifecycle methods: `#[setup]` (mandatory, builds all IO from
  `SetupContext<Self>`), `#[step(hz = N)]` (scheduled control step, at most one),
  `#[shutdown]` (graceful park/flush before bus close, at most one).
  Query servers: `#[server(topic = …)]` (exclusive, holds `&mut self`, serialized
  with `#[step]`), `#[server_snapshot(topic = …)]` (concurrent, reads a committed
  `Snapshot`), `#[snapshot]` (the committed-snapshot provider).
- Handles are typed fields on the `Api` struct: `Publisher<B>`, `Subscriber<B>`,
  `Latest<B>`, `Querier<Req, Resp>`, or a `Vec`/`BTreeMap` of one for per-instance
  IO.
  Every body must implement `ContractBody`, and the derived `Api` records each
  field's own generation-qualified contract identity.
  A field using the wrong contract type or a setup handle not declared by the
  `Api` struct is a compile error
  ([`phoxal/src/participant/context.rs`](../phoxal/src/participant/context.rs)).
- The entrypoint is plain Rust: `fn main() -> phoxal::Result<()> {
  phoxal::run::<Participant>() }`.
  The runner owns the clap/env launch contract, robot-model loading, the bus
  connection, the clock, step scheduling, server dispatch, snapshot commits, and
  shutdown. There is no runtime introspection subcommand: a consumer that needs
  a participant's contract surface reads its embedded linker-section metadata
  instead of executing it
  ([`phoxal/src/participant/metadata.rs`](../phoxal/src/participant/metadata.rs)).
- Official services take no typed config and build their state from the robot model
  via `ctx.robot()` (`Config::from_robot(&Robot)` in the service's own code - there
  is no shared resolver abstraction).
  User participants declare `config = path::Config` and read it as `Self::Config` in
  `#[setup]`.

## Components

- Component capability contracts live under the api tree's `component(instance)`
  node ([`phoxal-api/src/lib.rs`](../phoxal-api/src/lib.rs)); each `kind(capability)`
  child is a self-contained node whose key is
  `component/<instance>/<kind>/<capability>/<leaf>`.
- A component driver is an ordinary participant launched once per
  `robot.components.<instance>` entry with `#[phoxal::driver(id = "…")]`; the
  bound instance is provided via `ctx.component()`, and the driver derives its
  per-instance handles from the robot model.
- `component.yaml` is the only source of component-local capability definitions;
  `robot.yaml` does not override component-local fields.
  `robot.yaml` component entries are instance-only (type, mount link, per-instance
  `driver` config with an explicit `connection`, sparse per-capability `parameters`).
- Keep component-local ids local in the component source; namespace them only at
  robot composition boundaries (`<instance-id>.<capability-id>`, the `CapabilityRef`).
  The `CapabilityRef` is distinct from the bus key - the manifest binding maps a
  `CapabilityRef` to the concrete `component/…` key.
