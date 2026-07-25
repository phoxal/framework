# Framework Conventions

Engineering conventions for the bus, topics, logical time, participant authoring, and
components inside this workspace.
Rust style is in the org-level
[RUST_GUIDELINES](https://github.com/phoxal/organization/blob/master/docs/engineering/RUST_GUIDELINES.md);
contract discipline is in [CONTRACTS.md](./CONTRACTS.md).

## Bus and contracts

- Use `phoxal::bus` plus the train-selected `phoxal::api` module for all inter-service
  communication.
  Do **not** add a direct `zenoh` dependency outside `phoxal::bus`, except in
  `tool/bus` for sample observation over its runner-owned session and in
  `infrastructure/router`, which owns the transport process rather than joining
  the participant graph.
- `phoxal::bus` owns the Zenoh session/builder (`Bus`), the body-typed handles
  (`Publisher`, `Subscriber`, `Latest`, `Querier`), the query/server primitives,
  and the wire ABI: the topic-key scheme, the codec, the encoding string, and the
  `BusMetadata` attachment.
  Service, driver, tool, and simulator code connect through the runner-owned bus
  (they do not open Zenoh themselves) and name topics through the api modules.
  The infrastructure router is the sole direct session-opening exception.
- The wire body is the **plain MessagePack payload** of a version-local body type.
  Compatibility keys on exact name identity (D1): the version is folded into
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

Topic keys are **version-qualified** and api-local; the api tree's `topic` builders are
the only source of keys, and the wire body never appears in the key
([`phoxal-api/src/lib.rs`](../phoxal-api/src/lib.rs),
[`phoxal-bus/src/topic.rs`](../phoxal-bus/src/topic.rs)).

- Domain streams: `<revision>/<domain>/<stream>` (e.g.
  `<revision>/drive/state`, `<revision>/drive/target`), built as
  `api::topic::new().drive().state()`.
- Domain queries: a single `<revision>/<domain>/<query>` key carrying request + response
  bodies (e.g. `<revision>/frame/lookup`, `<revision>/map/submap`,
  `<revision>/asset/get`).
- Per-instance component capabilities:
  `<revision>/component/<instance>/<kind>/<capability>/<stream>` (e.g.
  `<revision>/component/front_left_drive/motor/motor/command`), built as
  `api::topic::new().component(instance).motor(capability).command()`.
  These are dynamic keys resolved from the robot model in `#[setup]`.
- The runner applies the multi-robot root `<namespace>/robots/<robot-id>/` to every
  version-qualified key at the transport layer; service code obtains that key
  only through its selected API version's typed builder.
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

- Robot services and drivers track state-transition time, watchdogs, and
  synchronous-input staleness with **logical time** - never wall time directly.
  The runner owns one `ClockSource` and stamps every `StepContext` and every
  `produced_at_ns` from it, so participants in the robot clock share one domain
  ([`phoxal/src/participant/clock.rs`](../phoxal/src/participant/clock.rs)).
- Tools are outside that clock. Their process launch contract has no `--clock`
  flag or `PHOXAL_CLOCK` binding, and the normal embedding API accepts no clock
  argument. They run from external events and host-monotonic timers in every
  mode. Tool envelope metadata uses `phoxal::raw::host_time()`; the runner never
  gives tools `StepContext` or enrolls them in logical scheduling. Official tool
  sources are checked against simulation-clock imports; privileged user-authored
  raw-bus tools must uphold the same rule and never decide freshness from robot
  logical time.
- Simulators are also not clock-selectable participants. Their process launch
  contract exposes no `--clock` or `PHOXAL_CLOCK`; Webots drives their execution.
  This does not remove the semantic simulation-time contract: the simulator
  controller publishes `simulation/clock` after each completed Webots step and
  clocked robot participants consume it.
- A logical-time consumer of asynchronous external input owns retention and
  freshness. Keep only the latest bounded value, record its consumer-local
  monotonic arrival instant, and sample that value at the logical step. A
  logical pause must not accumulate a replay backlog.
- `LogicalTime` is `{ epoch, time_ns }`. Epochs are opaque equality-only
  execution identities: any different epoch signals replacement, regardless
  of numeric direction. Within one epoch `time_ns` strictly increases.
  `RealClock` reads the host-wide UNIX-epoch domain (so cross-process staleness
  checks are comparable) and latches monotonically; `TestClock` is an injectable
  fake for tests.
- In `ClockMode::Simulation`, the runner subscribes to the Webots controller's
  authoritative version-qualified `simulation/clock` contract. Each received
  sample advances the
  scheduler to the envelope's logical time. If Webots does not step, the
  controller publishes nothing and participant scheduling remains still.

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
  defaults to `()` - tools stay raw-bus only and host/event driven, with no
  logical clock accessor or scheduled step);
  `#[phoxal::simulator]` for simulation-only participants.
  Each kind embeds its own JSON metadata (id, kind, contract surface) as a
  static in a dedicated linker section on the compiled binary
  (`.phoxal_api_meta` / `__phoxal_meta`), so a consumer reads it straight out of
  the object file without ever executing the artifact.
  Infrastructure binaries are deliberately outside these authoring kinds and
  must not embed participant metadata; release validation enforces that absence.
- Lifecycle methods: `#[setup]` (mandatory, builds all IO from
  `SetupContext<Self>`), `#[step(hz = N)]` (scheduled control step, at most one),
  optional async `#[reset]` (simulation execution replacement, serialized with
  steps and exclusive servers), and `#[shutdown]` (graceful park/flush before
  bus close, at most one). `#[reset]` receives `ResetContext`, may also take
  `&mut Self::Api`, and is unavailable to clockless tools.
  Official component drivers intentionally use the generated no-op reset:
  their mutable state is hardware-derived and remains owned by the hardware
  process rather than a replaceable simulated world. Simulator participants
  are structurally real-clock world owners, so their own reset hook is never
  driven by `simulation/clock`; Webots process replacement is their reset.
  Query servers: `#[server(topic = …)]` (exclusive, holds `&mut self`, serialized
  with `#[step]`), `#[server_snapshot(topic = …)]` (concurrent, reads a committed
  `Snapshot`), `#[snapshot]` (the committed-snapshot provider).
- Handles are typed fields on the `Api` struct: `Publisher<B>`, `Subscriber<B>`,
  `Latest<B>`, `Querier<Req, Resp>`, or a `Vec`/`BTreeMap` of one for per-instance
  IO.
  Every body must implement `ContractBody`, and the derived `Api` records each
  field's own version-qualified contract identity.
  Once an epoch is active, inbound buffers expose only its samples. Samples
  from a possible replacement epoch are quarantined in bounded per-epoch
  storage so controller outputs published before their matching clock can be
  promoted atomically at the boundary; they never replace active data early.
  Activation purges unmatched candidates and late samples from a retired epoch
  remain unobservable. Runtime buffer rows disclose these discards through
  `epoch_filtered`. A `Subscriber` or `Latest` carrying clockless host/operator
  intent opts out explicitly with `#[phoxal(epoch_agnostic)]`; producer handles
  cannot use that marker.
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
