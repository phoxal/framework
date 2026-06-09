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
| [`phoxal::bus`] | Typed Zenoh transport — pub/sub + query leaves, decode-error discipline. |
| [`phoxal::model`] | Authored manifest schemas: `robot.yaml`, `structure.urdf`, `component.yaml`, `simulation.yaml`. `model::v1::Robot` is the resolved superset. |
| [`phoxal::spatial`] | Geometry, frames, transforms, risk model. |
| [`phoxal::api`] | The typed bus wire contracts — one module per platform runtime (`drive`, `localize`, `mission`, …), plus `component`, `presence`, `simulation`. Each leaf exposes `path()` / `topic()` / `publisher`-`subscriber` (or `get`-`queryable`) via the `topic_leaf!` macro. |
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

The platform runtime *binaries* (`phoxal-runtime-<name>`) ship as container
images, not crates — they depend on this crate but are not published here.

## Features

- `scenario` — the Webots validation harness (off by default so production
  runtime images don't carry test code).

## Status

Pre-1.0, building in public. The API evolves inside `pub mod vN` modules of each
`phoxal::api::<name>`; breaking changes are loud, not quiet.

## License

AGPL-3.0-only. A commercial license is available — see the repository.
