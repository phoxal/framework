# Framework Conventions

Engineering conventions for the bus, topics, logical time, runtime bootstrap, and
components inside this workspace. Rust style is in the org-level
[RUST_GUIDELINES](https://github.com/phoxal/organization/blob/master/docs/engineering/RUST_GUIDELINES.md);
contract discipline is in [CONTRACTS.md](./CONTRACTS.md).

## Bus and contracts

- Use `phoxal::bus` plus owner-local `phoxal::api::<name>` modules for all
  inter-service communication. Do **not** add a direct `zenoh` dependency outside
  `phoxal::bus`.
- `phoxal::bus` owns the Zenoh-backed builder, `Bus`, typed transport
  primitives, lazy/eager publisher policy, schema encoding, and query-retry
  mechanics. Runtime, driver, simulator, and tool crates connect through the bus
  builder, then use the api modules for semantic topic and query contracts.
- Endpoints use Zenoh endpoint syntax directly (`tcp/127.0.0.1:7447`,
  `tcp/router:7447`); `phoxal::bus` does not repair bare hosts, `tcp://…`
  URIs, or missing ports.
- By convention each topic has one public producer: products (state, telemetry,
  debug) are read via a subscriber and commands are sent via a publisher, with the
  opposite side owned by the runtime/driver/tool that produces the effect. (The
  bus leaf helpers expose both builders; the convention is about which side an
  owner publishes, not a type-level restriction.) Avoid `pub use` re-export
  shims — import from the concrete owning module path. Query retry is explicit at
  each caller.

## Topic naming

- Runtime streams: `runtime/<name>/<stream>` (explicit names like `state`,
  `command`, `health`, `image`, `scan`, `ticks` — never prefix/suffix splicing).
- Runtime debug products: `runtime/<name>/debug/<product>`.
- Runtime queries: `runtime/<name>/<query>` with `/request` and `/response`
  suffixes (e.g. `runtime/asset/get`, `runtime/frame/lookup`,
  `runtime/video/open`).
- Simulation domain: `simulation/<stream>` and `simulation/<query>`.
- Capability streams: `component/<component-id>/<capability-kind>/<capability-id>/<stream>`
  (some streams, e.g. profile specs, omit the kind segment). Capability api
  crates expose unrooted `topic(...)` helpers for the concrete path.

## Pub/sub and telemetry

- Every pub/sub payload uses `phoxal::bus::pubsub::Stamped<T>`. Bare payloads
  on a topic — including commands and heartbeats — are bugs. Query
  request/response payloads are not `Stamped` unless the queried data itself needs
  an event time.
- Publishers are **lazy by default** (no subscriber → skip serialization). Use the
  eager path only for topics that must publish regardless of matches
  (command/backbone flows).
- When a runtime needs to explain a decision, publish a typed, runtime-owned
  debug product under `runtime/<name>/debug/<product>`: `Stamped<T>`, logical
  time, small and bounded, lazy. If the information is useful to another runtime's
  behavior, make it a primary semantic topic instead of a debug product.

## Logical time

- Runtimes and drivers track time, watchdogs, staleness, and computations with
  **logical time** only — never wall time (`Instant::now`, `SystemTime::now`,
  `.elapsed`). Use the engine/simulator clock (`phoxal::runtime`'s sim clock
  carries `epoch`, `step`, `time_ns`, `dt_ns`).
- Simulation uses next-step visibility: messages produced during step `S` are
  consumed at `S+1`. In simulation mode, subscriptions buffer stamped inputs and
  `step` is the only place that mutates logical state or publishes step-owned
  outputs. Reset local dedupe state on epoch change; ignore stale/duplicate steps.
- The Webots supervisor owns epoch generation and publishes the shared
  `simulation/clock`. Reset is a command-with-ack that starts a new epoch.

## Runtime bootstrap

- Runtime crates (`runtime/<name>`) and component drivers are primarily binaries
  (`phoxal-runtime-<name>`); some expose a small library target for shared
  selector/scenario logic. `main.rs` stays a thin entrypoint.
- Express every runtime through the shared `Runtime` trait and run it through the
  engine's `execute(...)` (`phoxal::runtime::execute`). The trait owns CLI args, config
  resolution, the step-loop clock period, and the scenario surface; the harness
  provides the common `run` / `scenario` / `scenarios` subcommands.
- Register inbound surfaces at construction: pub/sub via the bus subscribe
  helpers, queries via a `serve_query` surface answered off the step loop from the
  last committed read view (a pure `fn(&View, Req) -> Resp`). `step(...)` consumes
  an input batch and returns `Result<()>`. Outputs are stored publisher handles
  created at construction (normal vs eager) and called from `step`; do not return
  publish batches from `step`.
- A pure query service (e.g. `runtime/asset`) is just a `Runtime` that registers a
  query surface with an empty `step`. Shared bootstrap mechanics live in the
  engine, not duplicated per binary.

## Components

- Component capability contracts live in `phoxal::api::component`. A
  capability-bearing component has an executable driver; a component with no
  capabilities declares no per-instance driver config.
- `component.yaml` is the only source of component-local capability definitions;
  `robot.yaml` does not override component-local fields. `robot.yaml` component
  entries are instance-only (type, mount link, per-instance `driver` config with
  an explicit `connection`, sparse per-capability `parameters`).
- Keep component-local ids local in the component source; namespace them only at
  robot composition boundaries (`<instance-id>.<capability-id>`). Geometry shared
  across runtimes lives in `phoxal::spatial`.
