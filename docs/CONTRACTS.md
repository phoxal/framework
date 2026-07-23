# Contract Discipline

The cross-cutting rules every contract in the `phoxal_api` tree follows.
The per-domain contracts (the actual payload/query bodies for drive, motion,
navigation, map, perception, localize, sensor capabilities, …) are all declared in one
`phoxal_api_tree!` invocation in [`phoxal-api/src/lib.rs`](../phoxal-api/src/lib.rs);
this file is the shared discipline they obey.

For the architecture these contracts serve, see the org-level
[ARCHITECTURE](https://github.com/phoxal/organization/blob/master/docs/product/ARCHITECTURE.md)
and [BLUEPRINT](https://github.com/phoxal/organization/blob/master/docs/product/BLUEPRINT.md).

These rules are the target contract discipline the workspace is converging to
(pre-1.0).
Where a runtime's current implementation lags, the rule is the direction, not a
claim that every handler already enforces it.

## Train-selected concrete API identity

There is no version-tagged wire enum and no `{"v":…,"data":…}` body wrapper.
A participant's `#[derive(phoxal::Api)]` handle struct names bodies through
`phoxal::api`. The resolved framework train selects one complete concrete API
revision for the official graph.
The graph is compatible **per contract** by exact version-qualified name
identity (D1), not by a wire-shape hash.

- API revisions are conventional **`v<major>_<minor>` modules** in the `phoxal-api` crate,
  beginning with `phoxal_api::v0_1`, each with a zero-variant marker
  `enum Api {}` implementing `ApiVersion { const ID }` (the canonical version
  string, `"v0.1"`).
- Contract bodies are **version-local plain serde structs/enums**
  (`api::drive::State`, `api::drive::Target`), each with a generated `ContractBody`
  impl that fixes its `Api`, `NAME`, `VERSION`, `CONTRACT`, and `TOPIC`. There
  is no `SCHEMA_ID`/`FAMILY`: the version is folded into `TOPIC` itself, so two
  differently-versioned contracts are physically distinct Zenoh keys and cannot
  collide.
- One macro - **`phoxal_api_tree!`** (in `phoxal-macros`) - owns the whole tree:
  the version modules, the bodies, the topic keys, the pub/sub vs query
  kind, and the api-local `topic` builders.
- A child revision may `extends` exactly one earlier revision. The macro
  materializes a complete child tree, regenerating inherited bodies under the
  child's concrete identity and re-rooting inherited `crate::<parent>::...`
  type references to the child. Additions are direct; replacement and removal
  are explicit. Exactly one final `latest` declaration selects `phoxal::api`.
- A topic body belongs to one node and one topic. Sibling topics duplicate
  request/response bodies rather than sharing their `ContractBody` identity.
  A parent may define a plain, non-topic protocol value for children to
  reference through an absolute crate path; `crate::v0_1::tool::Cursor` is the
  deliberate example shared by the separate log and bus retention protocols.

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
- Participant startup presence is an exact process-incarnation fact. The
  Liveliness token key is
  `<robot-root>/liveliness/participants/<participant-id>/<incarnation>`;
  readiness waits for the launched incarnation, while UI presence aggregates
  all live incarnations by stable participant id.
- A body **may** carry an additional, explicitly named time field only when it
  denotes a different instant than produce time - `measured_at_ns` (sensor sample),
  `expires_at_ns` (when something lapses).
  Several capability `Sample` bodies already do this.
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
  framework owns, as with `drive::StopReason` and `navigation::RefusalReason`.
  A bare `String` is not an acceptable reason for a degraded, stopped, refused, or
  error state.
- A reason field is present only when consumers branch on it.
  If the variant alone drives behavior, no reason is added.
- Human-readable explanatory text lives in an `Option<String> detail` alongside the
  typed reason (as in `navigation::Outcome` and `power::State`),
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
- Bounded retention tools use a complete snapshot query plus a live follow
  topic. Their cursor combines an opaque process generation with a monotonic
  ingest sequence. Consumers buffer follow items while querying, install the
  snapshot, replay only newer buffered items, and re-query on a generation
  change or sequence gap. `v0.2::tool::log` retains the newest 1,000 existing
  `v0.2::logs` events; `v0.2::tool::bus` retains the newest 60 one-second windows.
  `v0.2::tool::runtime` retains five host-monotonic minutes of portable runner
  rollups behind a bounded, participant-filterable backward-paginated query.
  Ingest clamps participant ids to 512 bytes, exact topic ids to 256 bytes, and
  normal rows to 256 plus an explicit aggregate overflow row. Retention has
  both record and byte caps; capacity evictions and identity truncation remain
  visible in the response. The published `v0.1` concrete revision remains
  immutable. Its follow stream uses the same generation/sequence recovery rule.

### Raw retention-tool coherence boundary

The retention tools serve `v0.2::tool::{log,bus,runtime,device}` through their
raw-bus owner capability. This is an explicit current coherence gap: tools are
intentionally clockless, raw-bus-only participants and their authoring model
fixes `Api = ()`, so these served query/state edges are not embedded in the
participant metadata that `cargo xtask coherence-check` reads. Pretending
otherwise with a parallel hand-maintained metadata format would create a
second contract authority.

Until tool authoring gains a dedicated served-API metadata surface, the proof is
instead the single `phoxal_api_tree!` declaration (which owns body and topic
identity), its exact topic-key and MessagePack round-trip tests, and the
owner-capability constructors used by both servers/publishers. A migration of
any retained surface must update its raw producer and consumer together; the
coherence gate cannot currently detect a stranded consumer for these raw
edges. This limitation is canonical and deliberate, not a claim that the raw
surfaces participate in coherence today.

## Revision linkage

Stateful products that depend on an upstream map/localization state carry the
**upstream revision** they were produced under rather than minting their own id.
Today this is a plain `Option<u64>` field on `navigation::Path` and
`navigation::Frontier`, linked to `map::Revision.revision`, so a consumer can
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

Published concrete revisions are immutable. Breaking changes create a new
major/minor revision extending one earlier parent; the development branch and
pull request are the only preview boundary.

There are no per-service independent semver tracks for contracts and
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
