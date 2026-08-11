# xtask

The workspace's command runner.
`cargo xtask <verb>` is aliased in [`.cargo/config.toml`](../.cargo/config.toml).

```
cargo xtask compatibility report          # what changed, and what release it needs
cargo xtask compatibility check-release   # the same, and fail if this version is too small
cargo xtask compatibility rehearse-v1     # drill the Stable-line semantics, offline
cargo xtask compatibility v1-readiness    # everything that has to hold before a 1.0
```

`report` and `check-release` accept `--declared-impact <unchanged|additive|breaking>`.
`v1-readiness` accepts `--allow-rust-version-raise`.

## What the checker proves

It compares the **contract surfaces** this workspace declares against the ones the latest published framework train declares, and classifies the difference:

- `unchanged` - every record the baseline declared is still declared, identically, and nothing was added.
- `additive` - records were added and every record the baseline already had is untouched.
- `breaking` - a record the baseline declared is gone or is declared differently.

A record is one endpoint, one schema-tagged document, one out-of-body envelope, one exact wire constant, or one launch contract.
Each contract-owning crate renders its own records, so the checker reads a declared process boundary rather than guessing one from the outside.

The impact then selects the smallest release that may carry it.
Before 1.0 an addition and a break both land on the next minor, because SemVer gives a 0.x release no major axis to break on; from 1.0 on a break is a major.
`check-release` fails only when the workspace version under-states what changed - releasing further than required is always allowed.
`report` never fails on the verdict, which is what makes it usable on an ordinary pull request that is not setting a version.

## The toolchain floor is the second axis

The contract surfaces are what a *peer* speaks; `rust-version` is what a *consumer* needs to build the same crates.
Raising it breaks every downstream build still on the previous toolchain, which is a break of the same kind as a removed endpoint: the consumer did nothing and stopped working.

So both verbs also compare the workspace's `[workspace.package] rust-version` against the floor the published train went out under, read from the same sparse-index entries the baseline is resolved from.
A raised floor requires at least a minor, in both SemVer eras: pre-1.0 the minor is the line, and from 1.0 on a raised floor is a minor by the convention the wider ecosystem already reads that way.
A lowered or equal floor asks nothing of anybody and constrains nothing.
All five contract crates are built from one workspace and inherit one `rust-version`, so a published train stating two floors is refused the same way a half-published train is: it is not one train.

## When a gate fails

The gate names what it found and points here.
These rules say what may be fixed without asking, and what stops.
An agent responding to a red gate follows the rule the failure names and does not improvise a way past it.

### 1. The impact is `breaking` and the break is intended

Carry the Conventional Commit breaking marker (`feat!:`, `refactor!:`) on the pull request title so the release automation sizes the next train as a line move, then re-run the gate.
The candidate then clears the minimum the checker states.
Pre-1.0 nothing else is required: a line move is an ordinary release, and no further approval applies.

### 2. The impact is `breaking` and the break is not intended

Revert the wire change.
A bigger version is not a remedy: raising the candidate until the gate passes converts an accident into a published contract change that every peer has to follow.
The gate is satisfied by the change going away, never by the version growing to cover it.

### 3. A frozen bootstrap fact drifted

Stop.
The frozen set is the `supervisor/connect` key spelling, both bootstrap document shapes, the canonical framework-version spelling inside them, and the bootstrap-reachable transport facts: discovery, key grammar, query envelope, encoding, zenoh wire protocol.
Those five are everything an attaching client traverses *before* it can decode the bootstrap reply, so a line that moved one of them would leave the frozen documents intact and unreachable.
Concretely they are the `phoxal-bus` records `bus-key-root`, `bus-key-composition`, `encoding`, `zenoh-wire-protocol`, `BusMetadata` and `QueryFailure`, plus the discovery mechanics and the transport version their own pin tests hold.
A report marking a record `[FROZEN BOOTSTRAP]`, or a failing bootstrap pin test, means one of them moved.

A Zenoh upgrade that changes the wire protocol version lands here too.
It is not a routine dependency bump: peers that disagree on it never form a session, so nothing above it ever gets the chance to report the disagreement.

Do not fix forward.
Do not adjust the pin to match what the code now produces.
Do not proceed with a release.
Escalate to the maintainer with the diff.
There is no autonomous remedy for the frozen set: it is what two binaries exchange before they know whether they agree on anything else, so a peer built from any line has to decode it, including lines that do not exist yet.

### 4. The `rust-version` floor rose

Either revert the raise, or acknowledge it and release at least a minor.
Name which crates forced it: run `cargo check --workspace` on the previous floor, or read the dependency whose own `rust-version` moved, and state that dependency in the release notes.
An accidental raise is usually one dependency bump, and reverting that bump is the smaller change.
`v1-readiness` takes the acknowledgement as `--allow-rust-version-raise`; the release-sizing gate takes it as a candidate version that is at least a minor.

### 5. Post-1.0: the change has to ship inside an existing line and the checker calls it breaking

There is exactly one legal shape.
Add a new independent endpoint beside the existing one, leave the existing one exactly as it is, and put the choice between them in the client as a capability check or an adapter.
Nothing already published moves, so no peer on the line is broken, and the new endpoint is an addition that a minor carries.

If that shape does not fit the change, the change does not belong inside the line.
Escalate to the maintainer before any release; do not reshape the existing endpoint and do not declare the impact smaller than it is.

### 6. The surface output changed for contracts nobody edited

The introspection machinery changed, not the contracts.
Treat it as breaking, because that is what every consumer of the comparison now sees.
Fix the generator so an unchanged contract renders unchanged output; that is the remedy in almost every case, and it is what keeps one refactor from reporting a break in every record at once.
If the rendering genuinely has to change, it changes only in a release that is itself allowed to move the line, and that is a maintainer's decision: escalate.

### 7. The candidate version under-states an unchanged or additive change

Raise the candidate to the minimum the gate states.
No approval is needed and nothing else has to change: the contracts are already compatible, and the version is simply smaller than the release it is carrying.

## What it cannot prove

It proves **structure, not meaning**.
It sees a renamed field, a removed endpoint, a payload whose shape changed, a launch argument that stopped being required.
It cannot see a meaning that changed under a shape that did not: a distance that starts arriving in centimetres, a frame convention that flipped, a status code that was given a new interpretation.
Every one of those breaks a peer while the surfaces compare identical.

`--declared-impact` is where such a break is stated.
The effective impact is the greater of the mechanical one and the declared one, so a declaration **only ever raises** it: nothing can talk a real break down to an addition.
Naming the semantic change in the source and in review remains the author's job; this command is a floor under that judgment, not a replacement for it.

## The published crates are the only baseline

Nothing is stored in the repository for a comparison to read.
A snapshot committed beside the code would be updated by the same change it is supposed to judge, and a reviewer would then be asked to notice a baseline edit hidden inside a feature diff.
The registry cannot be rewritten, so it is the only honest record of what shipped.

The baseline is the newest non-yanked version that **all five** contract crates share - `phoxal`, `phoxal-api`, `phoxal-bundle`, `phoxal-bus`, `phoxal-runtime-contract` - read from the crates.io sparse index.
A train publishes as one set; when the crates disagree the checker names the partially published train and stops rather than comparing one crate's contracts against another crate's previous release.

Both sides are read the same way: a probe project under `target/xtask-compat/` depends on the five crates (baseline at exact `=x.y.z` registry versions, workspace by path) and prints their surfaces as one JSON document.
Since both sides go through one mechanism, a difference in the output is a difference in the contracts and not in how they were read.
The probes are regenerated byte-identically on every run, so Cargo rebuilds them only when the crates underneath them changed.

## The first comparable baseline

A crate that predates the contract-surface module has no surface to state, so the probe against it does not compile.
That is reported as `no comparable baseline`, and both verbs pass: there is genuinely nothing to compare, and a vacuous pass dressed up as `unchanged` would be a lie.
Every train published before the first surface-carrying one is in that state.
Once one is published the outcome cannot recur, because a published crate is never rewritten.

## Evolving the generator

Within one compatibility line, a change to the surface rendering must not alter the output for an unchanged contract: a refactor that moved bytes would report a break that never happened, in every comparison at once.
Change the rendering only together with a deliberate re-baseline - that is, in a release that is itself allowed to move the line.
The same rule governs record identity: whoever adds a record kind states in the same change how one record of that kind is named, and the checker refuses a kind it cannot identify rather than guessing.

## Rehearsing the line that does not exist yet

The framework is pre-1.0, so every Stable-line code path - a break needing a major, two versions on one `1.x` line interoperating, the freeze surviving a major flip - runs only inside `cargo test`.
A branch exercised only by a suite is a branch nobody has watched behave.

`cargo xtask compatibility rehearse-v1` runs the whole Stable matrix as one command and prints a PASS/FAIL row per fact:

- the release arithmetic and the sufficiency gate at Stable baselines, driven through the real checker against stated surfaces (1.4.x against 1.9.x on one line, 1.x against 2.x across lines, unchanged/additive/breaking, declared-impact escalation, the toolchain floor);
- `FrameworkVersion`'s own Stable semantics and the frozen bootstrap invariants, by running the pinned tests that own them as a subprocess.

Those last two are run rather than reimplemented, and they are run with `--exact` against a stated count.
The checker depends on no framework crate, so a copy of `FrameworkVersion` or of the frozen documents inside it would prove only that the copy agrees with itself; and a filter matching nothing exits zero, so the count is what stops the drill going green by covering less.

The drill reaches no registry: it proves what the semantics do at a Stable version, not which version is published.
CI runs it weekly on a schedule and on demand through `workflow_dispatch`, so a Stable path that stops working is found in the week it breaks rather than on flip day.

## Flip day

`cargo xtask compatibility v1-readiness` is the gate for the release that becomes 1.0.
It runs the rehearsal, then checks the conditions that only matter once: the workspace toolchain floor is the published train's (or the raise is acknowledged with `--allow-rust-version-raise`), the frozen bootstrap pin tests exist and pass, and no policy language that holds only below 1.0 is left in the tracked workspace.

That last check is a short list of phrases, each with the reason it becomes false.
A statement naming both eras is fine and is how these facts should be written: "pre-1.0 the line is the minor, from 1.0 on it is the major" stays true forever.
A statement deriving a standing rule from the pre-1.0 era alone is not, and the gate names the file to rewrite.

The command exits zero only when every machine-decidable condition holds, and it always ends by naming what no gate can decide: the flip itself is the maintainer's act.

## Tests

The classification, the release arithmetic and the baseline resolution are proved with stated fixtures, so `cargo test` reaches no network and compiles no probe.
The Stable matrix is proved the same way, by running the drill's own rows.
The one end-to-end proof lives in `tests/published_train.rs` and is `#[ignore]`d, because it queries the registry and builds the published crates:

```
cargo test --package xtask -- --ignored
```
