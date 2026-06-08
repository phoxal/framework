# Contract Discipline

The cross-cutting rules every `phoxal::api::<name>` module follows. The **per-domain
contracts** (the actual payload/query/command types for localize, map, frame,
mission, perception, safety, sensor capabilities, …) live under `phoxal::api` —
this file is the shared discipline they all obey.

For the architecture these contracts serve, see the org-level
[ARCHITECTURE](https://github.com/phoxal/organization/blob/master/docs/product/ARCHITECTURE.md)
and [BLUEPRINT](https://github.com/phoxal/organization/blob/master/docs/product/BLUEPRINT.md).

These rules are the **target contract discipline** the workspace is converging to
(pre-1.0). Where a runtime's current implementation lags, the rule is the
direction, not a claim that every handler already enforces it.

## Envelope and timestamp

Pub/sub payloads ride the `phoxal::bus::pubsub::Stamped<T>` envelope, which
owns exactly one time: when the message was produced (`timestamp_ns`).

- A payload struct must **not** carry a generic `timestamp_ns` — the envelope is
  authoritative for produce time. Duplicating it creates a drift bug and a
  "which is authoritative" ambiguity.
- A payload may carry an additional, **explicitly named** time field only when it
  denotes a different instant — `measured_at_ns` (sensor sample), `valid_at_ns`
  (the instant an estimate is for), `expires_at_ns` (when something lapses).
- Query request/response payloads are not `Stamped`; a query response that needs
  a time names it explicitly.

## Query results

A query result is a **single enum** (sometimes behind a thin transparent wrapper
type). The success variant carries the payload; each failure variant carries only
the diagnostics needed to recover (typically the current/served revision). No
result is both "failed" and "has data".

- Revision-linkage fields downstream consistency depends on (e.g.
  `served_map_revision`, `built_from_localize_revision`) live **inside** the
  success variant, never readable on a failure.
- Queries are **pure reads** — no observable side effect on runtime state. The
  only permitted touch is a self-scoped revision pin auto-released when the
  response completes. State changes use stamped pub/sub commands. A
  command-with-ack that changes state (e.g. `simulation/reset`) uses
  request/reply transport for the acknowledgement only and is namespaced as a
  command, never under a query namespace.

## Typed variants and closed-set reasons

State variants and failure reasons are typed enums, not strings.

- A "why" field (`reason`, `cause`, `failure_kind`) is a closed-set enum the
  framework owns. `String` is not an acceptable reason for a degraded, stopped,
  error, or rejected state.
- A reason field is present **only when consumers branch on it**. If the variant
  alone drives behavior, no reason is added.
- Public reason enums are `#[non_exhaustive]`, so adding a variant is a minor,
  not breaking, change.
- Human-readable explanatory text lives in debug products or logs, never inside
  the primary contract.

## Revision identifiers

Stateful runtimes (`localize`, `map`) own epoch-scoped revision ids —
`{ epoch: u64, sequence: u64 }` — for their state. Downstream products
(`perception`, traversability, …) carry the **upstream revision linkage** they
were produced under rather than minting their own id.

- `epoch` changes on reset / import / explicit lineage reset; `sequence`
  increases monotonically within an epoch.
- Comparing revisions across epochs is invalid except for equality/failure
  classification.
- Standard failure variants: `WrongEpoch`, `StaleRevision` (evicted same-epoch),
  `RevisionUnavailable` (future/unavailable same-epoch). Callers re-query the
  current revision after `StaleRevision`.

## Schema identity

Each `phoxal::api::<name>` module declares `SCHEMA_NAME` / `SCHEMA_VERSION` (e.g.
`phoxal-api-safety/v1`), and each typed contract carries a `TypedSchema`
`SCHEMA_NAME` that follows the contract path — `runtime/<name>/<stream>` for
pub/sub, with `/request` and `/response` suffixes for queries (e.g.
`runtime/safety/authorization`, `runtime/map/query/submap/request`).
`SCHEMA_VERSION` is numeric and starts at `1`. Every api crate carries a
contract-drift test asserting its `SCHEMA_NAME` and `SCHEMA_VERSION`.

## Large products

Pub/sub is for **bounded** products only. Use a revision-pinned query for large
products: pose-graph snapshots, large correction sets, submaps, ESDF/occupancy
tiles, traversability tiles, global grids, map snapshots. A large-product query
takes an optional `max_bytes` and returns a `ResponseTooLarge { available_bytes }`
variant rather than silently truncating or publishing an oversized payload; the
caller re-fetches by query.

## API evolution

Contract versions evolve inside `pub mod vN` modules. Additive changes go in the
existing `vN`; breaking changes mint `v(N+1)`; a domain may serve several `vN` at
once; removal happens at a major workspace bump. The framework workspace ships
all runtime contracts at one coherent version per release — there are no
per-runtime independent semver tracks.

A subscriber that decodes a payload it does not understand fails loud, carrying
the expected schema name and version. This is the framework's stand-in for a
compatibility manifest — contracts are statically known to consumers, so
mismatches surface at decode time rather than via a runtime descriptor protocol.
