# phoxal-project

`phoxal-project` owns the source-development boundary for Phoxal robot projects.

It discovers the nearest `robot.yaml`, requires the root Cargo package beside it, parses explicit `services:` and `connections:`, resolves exact dependency keys through Cargo metadata, prepares the mandatory supervisor dependency, and runs selected targets with one Cargo lock.

The crate deliberately has no dependency on the Runtime SDK, supervisor, service implementations, simulator, registry client, or archived `phoxal-cli`.

`cargo-phoxal` is the command-line entry point over this library.

PreparedProject::build_bundle builds the selected brain, services, component
drivers, and supervisor from the root Cargo graph, copies the runtime set into an atomic bin/ directory,
and writes deterministic manifest.json and provenance.json records.
When `robot.model` is authored, the bundle also carries its closed model/resource byte closure under `assets/` with per-resource digests and the closure digest in provenance.
Repeated unchanged assembly keeps the existing output directory and its
timestamps.
The records retain the authored robot document, package identities, target
selection, executable byte digests, and authored input digests without leaking
local source paths into package source identities.

The bundle is a source-side compiled product.
The supervisor executable is selected and built through the root graph but is
launched separately by `PreparedProject::run_local`.
`run_local` uses only the isolated `local` scope and `local` supervisor identity.
It does not imply protocol admission, domain readiness, or physical safety.
LocalRunPlan and LocalSimulationPlan retain the bundle and isolated local
identity without launching processes.
