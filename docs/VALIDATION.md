# Validation

How the framework proves its contracts — not just that a robot moves. Validation
must exercise the runtime contract end to end. The scenario spec and report
types live in `phoxal::scenario` (feature `scenario`); per-runtime
scenarios live in each `runtime/<name>` crate.

## Layers

1. **Schema.** Each authored file parses through its model module
   (`phoxal::model::robot`, `phoxal::model::structure`,
   `phoxal::model::component`) with
   parse + round-trip + `deny_unknown_fields` tests. Errors point at file + line.
2. **Resolution.** Each runtime process loads the source-shaped staged bundle
   (`robot.yaml`, `structure.urdf`, `components/<name>/component.yaml` + sibling
   files), calls the shared deterministic resolver, and extracts only its own
   typed slice. Identical inputs produce identical slices; every runtime input has
   an owner and every output has a schema. There is no generated per-runtime
   config file and no deploy descriptor; build reproducibility is provided
   downstream by the consumer CLI (`phoxal/phoxal-cli`) via `phoxal.lock` (image
   digests, component SHAs, tool hashes), not by this workspace.
3. **Contract.** Each `phoxal::api::<name>` module carries drift, decode, and
   actionable-error tests (see [CONTRACTS.md](./CONTRACTS.md)): the schema-family
   path and the contract enum's `{"v":…,"data":…}` wire shape are pinned (golden
   wire test), query responses are enums, revision linkage lives in the success
   variant, reasons are closed-set typed enums, payloads carry no generic
   `timestamp_ns`.
4. **Backend.** Replaceable runtime backends (notably localization) prove state
   cadence, reset/epoch handling, timestamp handling, revision monotonicity,
   correction behavior, query behavior, and mode transitions against the shipped
   v1 reference backend (**builtin odometry/dead-reckoning**) on synthetic and
   recorded inputs. ORB-SLAM3 and GNSS anchoring are experimental/unimplemented
   in this workspace and must not be advertised as resolved backend facts until
   corresponding runtimes and validation exist. A backend is profile-complete
   only after it passes this suite.
5. **Scenario.** Webots-backed gates from the framework's scenario catalog,
   organized into tiers and mapped to delivery phases (below).

## Scenario surfaces

Two surfaces, never sharing scenario definitions:

- **Framework conformance** — this workspace. A closed, phase-mapped catalog of
  framework-owned scenarios run by the scenario harness (in CI, headless by
  default). Assertions are generic ("frame transforms compose", "revision linkage
  holds"). They use framework-owned fixtures under `fixture/` and gate the
  delivery phases below.
- **Robot acceptance** — a robot repo (e.g. `phoxal/robot-rover`). An open catalog
  the robot owner authors, run via the consumer CLI (`phoxal/phoxal-cli`, external
  to this workspace) against that robot's worlds; assertions are model-specific. It
  gates the robot owner's release, not the framework's delivery phases.

## Tiers and delivery phases

| Tier | Scope | Purpose |
|---|---|---|
| 0 | schema, resolution, unit tests | fast contract validation (in `cargo test`) |
| 1 | headless contract scenarios | runtime contracts, basic system behavior |
| 2 | autonomy smoke scenarios | localization/map/plan/follow/safety flow |
| 3 | full autonomy + deploy preflight | release/deployment confidence |
| 4 | hardware, log replay, sim-to-real | real-world validation and regression |

A phase is complete only when **all** its categories pass together at the
required tier; passing a category in isolation is not enough.

| Phase | Atomic group | Categories (must pass together) | Tier |
|---|---|---|---|
| P0 | infra | boot-contract, readiness-bootstrap, resource-budget | 1 |
| P1 | proprioception | frame-calibration, odometry | 1 |
| P2 | spatial core (keystone) | localization, mapping, traversability, revision-convergence | 2 |
| P3 | safe degraded autonomy | safety (every localization mode), failure-recovery | 2 |
| P4 | directed navigation | planning, following | 2 |
| P5 | full autonomy | mission, exploration, perception (when the profile requires it) | 3 |

P2 is the keystone: P3–P5 depend on revision-linked map/localize state, so they
cannot be meaningfully validated until P2 passes as a group.

## Success-criteria philosophy

A scenario is complete only when it validates several independent facts. A
navigation scenario, for example, checks that mission accepted the command,
localization was in an allowed mode, the planner produced a revision-consistent
path, the follower consumed it, safety authorization stayed valid, drive
published bounded commands, simulator pose reached tolerance with no disallowed
contact, and the products could explain the behavior. Moving the robot is not
enough.

Scenario failures report the failed runtime/topic/query, relevant revision ids,
localization mode, mission state, safety decision, planner/follower state, and
simulator truth — actionable without reading raw logs first.

## Sim-to-real

Simulation passing is necessary but not sufficient. The stack also targets
recorded-sensor replay, hardware-in-the-loop checks, calibration replay, backend
conformance replay, and profile-specific log-replay corpora for conditions Webots
cannot reproduce honestly (e.g. outdoor/night illumination, GNSS degradation).
Simulation is the first gate because it is deterministic and cheap to repeat;
log-replay is the second gate for anything simulation cannot represent.
