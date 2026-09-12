# phoxal-project

`phoxal-project` owns the source-development boundary for Phoxal robot projects.

It discovers the nearest `robot.yaml`, requires the root Cargo package beside it, parses explicit `services:` and `connections:`, resolves exact dependency keys through Cargo metadata, and runs the selected root and service targets with one Cargo lock.

The crate deliberately has no dependency on the Runtime SDK, supervisor, service implementations, simulator, registry client, or archived `phoxal-cli`.

`cargo-phoxal` is the command-line entry point over this library.

PreparedProject::build_bundle builds each selected executable from the root
Cargo graph, copies the complete selected set into an atomic bin/ directory,
and writes deterministic manifest.json and provenance.json records.
Repeated unchanged assembly keeps the existing output directory and its
timestamps.
The records retain the authored robot document, package identities, target
selection, executable byte digests, and authored input digests without leaking
local source paths into package source identities.

The bundle is a source-side compiled product.
It is not an installed release, does not contain a supervisor unless that
executable is selected by the current graph, and does not imply process
startup, protocol admission, domain readiness, or physical safety.
LocalRunPlan and LocalSimulationPlan only retain the bundle and isolated
local identity for a future supervisor or simulator owner; they do not launch
processes.
