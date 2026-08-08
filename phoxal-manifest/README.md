# phoxal-manifest

Compiler for authored Phoxal project manifests. It parses project source files,
validates and resolves them, and produces a canonical `phoxal-model` robot plus
source-owned service/driver facts and deterministic assets for build tooling
such as `phoxal-cli`.

The persisted runtime artifact is owned by the sibling `phoxal-bundle` crate.
This compiler emits source-owned canonical values and deterministic asset
bytes for build tooling; it does not read or write `runtime.json`, and runtime
processes do not depend on this crate.

## Authored documents

`source` holds the exact serde shape of each authored document: `robot.yaml`,
`component.yaml` and `simulation.yaml`.
Each kind owns its schema version independently, so `robot.yaml` v1 can be introduced without forcing the other two to advance.
A new version is a new DTO beside the existing one with its own explicit conversion into the same crate-internal builder input, which is why adding one does not change anything `phoxal-model` exposes.

Every kind is reached the same way, through associated functions on its `Manifest`:
`parse` for exact text, `load` for a path (the document file, or the directory holding it), and `write_to_dir`.
All of them validate; there is no variant that parses without checking.
`robot.yaml` additionally composes the ordered parents it declares under `extends`, which is why only `load` resolves them - text handed to `parse` has no directory to resolve paths against.

`source::DocumentKind::generate` generates Draft 2020-12 editor schemas from those
same DTOs. They provide portable YAML completion and inspection only;
`phoxal validate` remains authoritative for strict YAML, semantic, cross-file,
and project-resolution validation.
The generated schemas are pinned by golden documents under `tests/golden/`, so a
DTO change that moves an editor schema has to be reblessed deliberately.
