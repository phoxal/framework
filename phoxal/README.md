# phoxal

The Phoxal robot framework as one crate: a coherent, manifest-driven stack for
production-oriented autonomous robots — typed bus, simulation-first validation,
and a complete platform-runtime contract set, with the same conventions from
authoring to deployment.

```toml
[dependencies]
phoxal = "0.4"
```

## Modules

| Module | What it is |
|---|---|
| [`phoxal::bus`] | Typed Zenoh transport. `Bus::{publish, subscriber, request, responder}` take a typed `Topic<Kind>`; payloads are plain `serde` data; schema identity + version travel in a `BusMetadata` attachment, so wire mismatches are loud decode errors. |
| [`phoxal::model`] | Authored manifest schemas: `robot.yaml`, `structure.urdf`, `component.yaml`, `simulation.yaml`. `model::v1::Robot` is the resolved superset. |
| [`phoxal::spatial`] | Geometry, frames, transforms, risk model. |
| [`phoxal::api`] | The typed bus wire contracts under `phoxal::api::v1::<domain>` (`drive`, `localize`, `mission`, …, plus `component`, `presence`, `simulation`). Topics are built with the fluent generated builder `phoxal::api::v1::topic`, e.g. `topic::new().v1().drive().target()` → `Topic<PubSub<Target>>`. Instance ids are method arguments (`component("base").motor("left").command()`); pass `phoxal::bus::topic::ANY` for subscribe wildcards (`component(ANY).motor(ANY).command()`). The leaf return type carries the payload, so the bus rejects a wrong payload at compile time. |
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
use phoxal::api::v1::{topic, drive::Target, drive::State};

// subscribe (wildcard ok): map (at_ns, payload) into the runtime's Input
io.subscribe_topic(topic::new().v1().drive().target(), InputPolicy::latest(), Input::Target).await?;

// publish: the timestamp is an argument, the payload is plain data
let state = io.publisher_topic(topic::new().v1().drive().state()).await?;
state.put(now_ns, &State { /* … */ }).await?;
```

The platform runtime *binaries* (`phoxal-runtime-<name>`) ship as container
images, not crates — they depend on this crate but are not published here.

## Features

- `scenario` — the Webots validation harness (off by default so production
  runtime images don't carry test code).

## Status

Pre-1.0, building in public. The API is a single version-first axis,
`phoxal::api::v1`; a breaking re-cut of the whole contract surface becomes
`api::v2`, while an individual contract's payload revision bumps its per-leaf
`version` in the `topic_tree!`. Breaking changes are loud, not quiet.

## License

AGPL-3.0-only. A commercial license is available — see the repository.
