# Contract Discipline

The cross-cutting rules every contract in the `phoxal_api` tree follows.
The per-domain contracts (the actual payload/query bodies for drive, safety, map,
mission, perception, localize, sensor capabilities, …) are all declared in one
`phoxal_api_tree!` invocation in [`phoxal-api/src/lib.rs`](../phoxal-api/src/lib.rs);
this file is the shared discipline they obey.

For the architecture these contracts serve, see the org-level
[ARCHITECTURE](https://github.com/phoxal/organization/blob/master/docs/product/ARCHITECTURE.md)
and [BLUEPRINT](https://github.com/phoxal/organization/blob/master/docs/product/BLUEPRINT.md).

These rules are the target contract discipline the workspace is converging to
(pre-1.0).
Where a runtime's current implementation lags, the rule is the direction, not a
claim that every handler already enforces it.

## One API version per participant; per-contract compatibility per graph

There is no version-tagged wire enum and no `{"v":…,"data":…}` body wrapper.
A participant authors against **one** API version; the graph may mix generations,
proven compatible **per contract** by `schema_id` agreement (#16).

- API versions are **dated modules** in the `phoxal-api` crate -
  `phoxal_api::y2026_1`, `phoxal_api::y2026_2` - each with a zero-variant marker
  `enum Api {}` implementing `ApiVersion { const ID }` (the canonical version
  string, `"y2026_1"`).
- Contract bodies are **version-local plain serde structs/enums**
  (`api::drive::State`, `api::drive::Target`), each with a generated `ContractBody`
  impl that fixes its `Api`, `FAMILY`, `TOPIC`, and `SCHEMA_ID` (the normalized
  transitive wire-shape hash - the per-contract compatibility key).
- One macro - **`phoxal_api_tree!`** (in `phoxal-macros`) - owns the whole tree:
  the dated version modules, the bodies, the topic keys + family ids, the pub/sub
  vs query kind, the `schema_id`s, and the api-local `topic` builders.
- A breaking change mints a new dated version; only the participants using a
  contract whose `schema_id` changed must move to it - it is not a per-contract
  body bump on the same bus, and not a whole-graph move either.

## Wire body and metadata placement

The wire body is the **plain MessagePack payload** of the version-local type - the
struct's fields, nothing more.
There is no envelope struct, no in-body timestamp, and no version tag in the body
or the key.

- `api_version`, `family`, and `codec` ride **bus metadata**: the Zenoh encoding
  string (a fast-reject hint) and the `BusMetadata` attachment.
  The canonical version identity is the body type's API module.
- Production time rides metadata too: `BusMetadata` carries
  `produced_at_ns` + `epoch` + `source { participant, incarnation, sequence }`
  ([`phoxal-bus/src/metadata.rs`](../phoxal-bus/src/metadata.rs)), so a payload
  struct never carries a generic `timestamp_ns`.
  A publish stamps these from the runtime's `LogicalTime`
  (`publisher.publish_at(at, body)`).
- A body **may** carry an additional, explicitly named time field only when it
  denotes a different instant than produce time - `measured_at_ns` (sensor sample),
  `expires_at_ns` (when something lapses).
  Several capability `Sample` bodies and `safety::SafetyAuthorization` already do
  this.
- Query request/response bodies carry no produce-time field; a query response that
  needs a time names it explicitly.

The `bus_abi` envelope (the Zenoh key layout + encoding-string format +
`BusMetadata` layout + codec id) is a separate, orthogonal framework-owned
constant; see [CONVENTIONS.md](./CONVENTIONS.md) and
[`phoxal-bus/src/abi.rs`](../phoxal-bus/src/abi.rs).

## Typed variants and closed-set reasons

State variants and failure reasons are typed enums, not strings.

- A "why" field (`reason`, `stop_reason`, a reason `code`) is a closed-set enum the
  framework owns, as with `drive::StopReason`, `safety::SafetyReasonCode`, and
  `plan::Refusal`.
  A bare `String` is not an acceptable reason for a degraded, stopped, refused, or
  error state.
- A reason field is present only when consumers branch on it.
  If the variant alone drives behavior, no reason is added.
- Human-readable explanatory text lives in an `Option<String> detail` alongside the
  typed reason (as in `safety::SafetyReason`, `mission::State`, `power::State`),
  never as the primary contract.

## Query contracts

Query topics declare a request and a response body on one topic
(`topic lookup: query LookupRequest => LookupResponse`).
The handle is `Querier<Req, Resp>` and the caller gets `Result<Resp, QueryError>`.

- A success reply is the plain `Resp` body - there is no Phoxal success envelope.
- A handler returns `ServerResult<T> = Result<T, QueryFailure>`, so the handler
  **selects the failure code** (`NotFound`/`InvalidArgument`/`Internal`/… ); the
  failure rides Zenoh's native `ReplyError` as a `QueryFailure { code, message,
  details?, details_encoding? }` ([`phoxal-bus/src/query.rs`](../phoxal-bus/src/query.rs)).
- `QueryError` (caller side) is `Unavailable | Timeout | Server(QueryFailure) |
  Decode | Protocol | TooManyResponders`.
  Timeouts are caller-owned (the `Querier`'s finite deadline, not Zenoh's 10 s).
  More than one responder on an exclusive topic is `TooManyResponders`; the querier
  does not take-first.
- When a response carries a domain outcome with no payload, model it as an enum
  body (e.g. `asset::GetResponse { Found { bytes }, Missing, InvalidPath }`) rather
  than overloading `QueryFailure`.
- Queries are pure reads.
  A `#[server_snapshot]` handler answers off the step loop from a committed,
  read-only `Snapshot`; an exclusive `#[server]` handler holds `&mut self` and is
  serialized with `#[step]`.
  State changes use pub/sub commands, not queries.

## Revision linkage

Stateful products that depend on an upstream map/localization state carry the
**upstream revision** they were produced under rather than minting their own id.
Today this is a plain `Option<u64>` field - `plan::Path.map_revision`,
`follow::Target { map_revision, built_from_localize_revision }`,
`explore::Frontiers.map_revision`, `map::Revision.revision` - so a consumer can
reject or re-query when the linkage does not match the state it holds.
A richer epoch-scoped revision id is a future contract change, not a current type.

## Large products

Pub/sub is for **bounded** products.
Large products (submaps, occupancy/ESDF tiles, pose-graph snapshots, large
correction sets) use a revision-aware query instead, as `map::submap` does
(`SubmapRequest` window -> `SubmapResponse` occupancy grid).
Stream-shaped sensor payloads (camera/depth/lidar frames) are the deliberate
exception: they ride their own per-instance capability topics under a low-priority
publisher profile, never inflating a control-state topic.

## API evolution

Additive changes within a dated version edit that version's `phoxal_api_tree!`
block in place.
A breaking change adds a new dated version (`version y2026_2 extends y2026_1`); the
macro generates a **fresh** Rust type per inherited contract (same `FAMILY`/`TOPIC`,
different `Api`), so an unchanged contract is wire-identical by construction yet a
distinct compile-time type.
Only the participants that use a contract whose transitive shape changed (a new
`SCHEMA_ID`) must move to the new generation; participants on unchanged contracts
keep interoperating across generations (#16). There are no per-service independent
semver tracks for contracts, and there is no mixed-version *decoding*: a shared
topic's producers and consumers must agree on the exact `schema_id`.

A subscriber fails loud on a body it cannot decode or whose metadata `schema_id`
does not match what the handle expects: the sample is counted
(`schema_mismatches`/`decode_errors`) and logged as a health signal, never silently
accepted ([`phoxal-bus/src/handle.rs`](../phoxal-bus/src/handle.rs)).
This decode-time loudness is the framework's compatibility backstop - contracts are
statically known to consumers, so mismatches surface at decode time rather than via
a runtime descriptor protocol.
