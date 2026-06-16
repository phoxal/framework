# phoxal

The Phoxal robot framework as one crate: a coherent, manifest-driven stack for
production-oriented autonomous robots — typed bus, simulation-first validation,
and a complete platform-runtime contract set, with the same conventions from
authoring to deployment.

```toml
[dependencies]
phoxal = "0.9"
```

## Modules

| Module | What it is |
|---|---|
| [`phoxal::bus`] | Typed Zenoh transport. `Bus::{publish, subscriber, request, responder}` take a typed `Topic<Kind>`; the wire body is the contract enum (it carries its own version via the variant), the versionless schema-family identity travels as the Zenoh encoding, and a `BusMetadata` attachment carries the produce timestamp — so wire mismatches are loud decode errors. |
| [`phoxal::model`] | Authored manifest schemas: `robot.yaml`, `structure.urdf`, `component.yaml`, `simulation.yaml`. `model::v1::Robot` is the resolved superset. |
| [`phoxal::spatial`] | Geometry, frames, transforms, risk model. |
| [`phoxal::api`] | The typed bus wire contracts under `phoxal::api::<domain>` (`drive`, `localize`, `mission`, …, plus `component`, `presence`, `simulation`). Each contract is a domain-first **enum** (e.g. `drive::Target` is `enum { V1(v1::Target) }`, `#[serde(tag = "v", content = "data")]`); the exact wire structs live under `phoxal::api::<domain>::v1`. Topics are built with the fluent generated builder `phoxal::api::topic`, e.g. `topic::new().drive().target()` → `Topic<PubSub<Target>>`. Instance ids are method arguments (`component("base").motor("left").command()`); pass `phoxal::bus::topic::ANY` for subscribe wildcards (`component(ANY).motor(ANY).command()`). The leaf return type carries the payload, so the bus rejects a wrong payload at compile time. |
| [`phoxal::runtime`] | The `Runtime` trait, `execute()` bootstrap, logical clock, runtime context, decision logging. |
| [`phoxal::scenario`] | Webots-backed validation harness (feature `scenario`). |

## Writing a user runtime

Implement the `phoxal::runtime::Runtime` trait and hand it to `execute()`; the
contracts you consume/produce come from `phoxal::api::*`:

```rust,ignore
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    phoxal::runtime::execute::<MyRuntime>().await
}
```

A runtime reads and writes contracts through the typed `Io` with fluent topics:

```rust,ignore
use phoxal::api::{topic, drive::{self, State}};

// subscribe (wildcard ok): map (at_ns, payload) into the runtime's Input.
// The payload is the contract enum — match its current variant:
//   match value { drive::Target::V1(target) => Input::Target(target) }
io.subscribe_topic(topic::new().drive().target(), InputPolicy::latest(), Input::Target).await?;

// publish: the timestamp is an argument; wrap the payload in the contract enum's
// current variant.
let state = io.publisher_topic(topic::new().drive().state()).await?;
state.put(now_ns, &State::V1(drive::v1::State { /* … */ })).await?;
```

The platform runtime *binaries* (`phoxal-runtime-<name>`) ship as container
images, not crates — they depend on this crate but are not published here.

## Features

- `scenario` — the Webots validation harness (off by default so production
  runtime images don't carry test code).

## Status

Pre-1.0, building in public. The API is domain-first: `phoxal::api::<domain>`
exposes a contract enum per wire type, and the enum *is* the wire body and the
single version authority. A payload revision adds a new variant (`V2`) — the
serde `rename` token is the version — while old variants stay so prior recordings
still decode. Breaking changes are loud, not quiet. See
[`src/api/README.md`](src/api/README.md) for the versioning rules.

## License

AGPL-3.0-only. A commercial license is available — see the repository.
