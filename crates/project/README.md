# phoxal-project

`phoxal-project` owns the source-development boundary for Phoxal robot projects.

It discovers the nearest `robot.yaml`, requires the root Cargo package beside it, parses explicit `services:` and `connections:`, resolves exact dependency keys through Cargo metadata, and runs the selected root and service targets with one Cargo lock.

The crate deliberately has no dependency on the Runtime SDK, supervisor, service implementations, simulator, registry client, or archived `phoxal-cli`.

`cargo-phoxal` is the command-line entry point over this library.
