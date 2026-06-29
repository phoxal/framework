# Validation

How the framework proves its contracts - not just that a robot moves.
Validation must exercise the runtime contract end to end.
Schema/resolution validation ships in this crate today (`cargo test`); the
scenario harness and tier/phase gates below are the validation roadmap the
workspace is converging to, not all wired up yet.

## Layers

1. **Schema.** Each authored file parses through its model module
   (`phoxal::model::robot`, `phoxal::model::structure`,
   `phoxal::model::component`, `phoxal::model::simulation`) with parse +
   round-trip + `deny_unknown_fields` tests.
   Errors point at the offending file/field.
2. **Resolution.** Each runtime process loads the source-shaped staged bundle
   (`robot.yaml`, `structure.urdf`, `components/<name>/component.yaml` + sibling
   files) and the shared deterministic resolver extracts the typed facts; official
   runtimes then build their own typed slice from the model
   (`Config::from_robot(&Robot)`).
   Identical inputs produce identical facts.
   There is no generated per-runtime config file and no deploy descriptor; build
   reproducibility is provided downstream by the consumer CLI
   (`phoxal/phoxal-cli`) via `phoxal.sources.lock` (resolved image digests,
   component commits, tool digests), not by this workspace.
3. **Contract.** The api tree carries golden + drift tests
   ([`phoxal/src/api/tests.rs`](../phoxal/src/api/tests.rs), see
   [CONTRACTS.md](./CONTRACTS.md)): the **plain** MessagePack body bytes are pinned
   (no `{"v":…,"data":…}` wrapper - the body is just the struct's fields), each
   body's `ContractBody` consts (`FAMILY`/`TOPIC`) and its `Api::ID` are asserted,
   topic keys + family ids are pinned, the encoding string + `BusMetadata` shape is
   pinned, query responses are typed bodies/enums, reasons are closed-set typed
   enums, and bodies carry no generic produce-time field.
4. **Backend.** Replaceable runtime backends (notably localization) prove state
   cadence, reset/epoch handling, timestamp handling, revision behavior, and mode
   transitions against the shipped v1 reference backend (**builtin
   odometry/dead-reckoning**) on synthetic and recorded inputs.
   ORB-SLAM3 and GNSS anchoring are experimental/unimplemented in this workspace
   and must not be advertised as resolved backend facts until corresponding
   runtimes and validation exist (`ResolvedLocalizeBackend` only resolves
   `DeadReckoning` today).
   A backend is profile-complete only after it passes this suite.
5. **Scenario.** Webots-backed gates organized into tiers and mapped to delivery
   phases (below).
   Scenario ownership is reassigned out of the runtime surface: scenario specs and
   the headless runtime harness are a separate concern (planned `phoxal::scenario`
   + `runtime::test::Harness`), discovery/orchestration belongs to `phoxal-cli`,
   and live-sim scenarios belong to the Webots tooling.
   The autonomy-profile model already names a scenario-coverage set per profile
   ([`phoxal/src/model/robot/v1/profile.rs`](../phoxal/src/model/robot/v1/profile.rs));
   the harness that runs them is not yet built in this crate.

## Scenario surfaces

Two surfaces, never sharing scenario definitions:

- **Framework conformance** - this workspace.
  A closed, phase-mapped catalog of framework-owned scenarios run headless by
  default, with generic assertions ("frame transforms compose", "revision linkage
  holds") over framework-owned fixtures under `fixture/`.
  They gate the delivery phases below.
- **Robot acceptance** - a robot repo (e.g. `phoxal/robot-rover`).
  An open catalog the robot owner authors, run via the consumer CLI
  (`phoxal/phoxal-cli`, external to this workspace) against that robot's worlds;
  assertions are model-specific.
  It gates the robot owner's release, not the framework's delivery phases.

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

P2 is the keystone: P3-P5 depend on revision-linked map/localize state, so they
cannot be meaningfully validated until P2 passes as a group.

## Success-criteria philosophy

A scenario is complete only when it validates several independent facts.
A navigation scenario, for example, checks that mission accepted the command,
localization was in an allowed mode, the planner produced a revision-consistent
path, the follower consumed it, safety authorization stayed valid, drive published
bounded commands, simulator pose reached tolerance with no disallowed contact, and
the products could explain the behavior.
Moving the robot is not enough.

Scenario failures report the failed runtime/topic/query, relevant revision ids,
localization mode, mission state, safety decision, planner/follower state, and
simulator truth - actionable without reading raw logs first.

## Sim-to-real

Simulation passing is necessary but not sufficient.
The stack also targets recorded-sensor replay, hardware-in-the-loop checks,
calibration replay, backend conformance replay, and profile-specific log-replay
corpora for conditions Webots cannot reproduce honestly (e.g. outdoor/night
illumination, GNSS degradation).
Simulation is the first gate because it is deterministic and cheap to repeat;
log-replay is the second gate for anything simulation cannot represent.
