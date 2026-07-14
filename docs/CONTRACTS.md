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

## Per-field API versions and per-contract identity

There is no version-tagged wire enum and no `{"v":…,"data":…}` body wrapper.
A participant's `#[derive(phoxal::Api)]` handle struct may mix contract types
from `v1` and preview `v2` field by field.
There is no participant-wide or graph-wide version pin.
The graph is compatible **per contract** by exact version-qualified name
identity (D1), not by a wire-shape hash.

- API versions are conventional **`vN` modules** in the `phoxal-api` crate -
  stable `phoxal_api::v1` and preview `phoxal_api::v2` - each with a zero-variant marker
  `enum Api {}` implementing `ApiVersion { const ID }` (the canonical version
  string, `"v1"`).
- Contract bodies are **version-local plain serde structs/enums**
  (`api::drive::State`, `api::drive::Target`), each with a generated `ContractBody`
  impl that fixes its `Api`, `NAME`, `VERSION`, `CONTRACT`, and `TOPIC`. There
  is no `SCHEMA_ID`/`FAMILY`: the version is folded into `TOPIC` itself, so two
  differently-versioned contracts are physically distinct Zenoh keys and cannot
  collide.
- One macro - **`phoxal_api_tree!`** (in `phoxal-macros`) - owns the whole tree:
  the version modules, the bodies, the topic keys, the pub/sub vs query
  kind, and the api-local `topic` builders.
- There is no `extends` or inheritance between versions. `v1` is frozen. `v2`
  is the one evolving preview surface and accumulates new or changed contracts
  in place until the version is ready to freeze.

## Wire body and metadata placement

The wire body is the **plain MessagePack payload** of the version-local type - the
struct's fields, nothing more.
There is no envelope struct, no generic in-body produce timestamp, and no
separate version tag in the body or metadata.
The version is part of the contract's topic key.

- The codec rides the Zenoh encoding string and the `BusMetadata` attachment.
  Contract version and identity do not ride metadata because they are already
  fixed by the subscribed topic key.
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

The Zenoh key layout, the encoding-string format, the `BusMetadata` layout, and
the codec id are a separate, orthogonal framework-owned wire ABI; see
[CONVENTIONS.md](./CONVENTIONS.md) and
[`phoxal-bus/src/abi.rs`](../phoxal-bus/src/abi.rs). Identity used to live in a
separately-maintained triple carried in the encoding string; that axis is gone
now that the version is folded into the Zenoh key itself, so `BusMetadata`
carries only provenance (codec, produce time, source) - never schema/family/
version.

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

Stable versions are immutable; `v1` is the current frozen surface. All work for
the next API goes into `preview version v2 { … }`. Additive and breaking changes
both edit `v2` in place while it is preview, so ordinary development does not
mint `v3`, `v4`, and so on. Once the complete `v2` surface is accepted it is
promoted and frozen; only then does later breaking work begin in preview `v3`.

There is no `extends`: a contract is declared in the version that owns its wire
identity. Participants may mix `v1` and `v2` contracts field by field during the
migration. There are no per-service independent semver tracks for contracts and
no mixed-version *decoding* on one topic: a shared topic's key is
version-qualified, so producers and consumers on it already use the exact same
name.

A subscriber fails loud on a body it cannot decode: the sample is counted
(`decode_errors`) and logged as a health signal, never silently accepted
([`phoxal-bus/src/handle.rs`](../phoxal-bus/src/handle.rs)).
This decode-time loudness is the framework's compatibility backstop - contracts
are statically known to consumers, and identity lives in the key itself, so
mismatches surface as an unreachable/unpublished topic or a decode error rather
than via a runtime descriptor protocol.
