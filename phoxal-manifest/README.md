# phoxal-manifest

Compiler for authored Phoxal project manifests. It parses project source files,
validates and resolves them, and produces a canonical `phoxal-model` robot plus
participant and asset declarations for tooling such as `phoxal-cli`.

`bundle` is the other half: the loader for a **finalized bundle**, the artifact
`phoxal-cli` writes and both the supervisor and every participant process read.
It builds the canonical model from the bundle's own finalized documents - there
is no second, compiled representation of a robot - and hands back the two
deliberately separate views of the bundle:

- `ParticipantAssetResolver` (from `phoxal-model`): the declared assets below
  `<bundle>/assets`, and nothing else. This is all a participant is given.
- `BundleResolver`: a safe index of every regular file in the bundle, `bin/`
  included, for the supervisor's remote `bundle/get`. Participants never get one.

The exact on-disk layout is documented on the `bundle` module and proven by
`tests/bundle.rs`, which stages a bundle from the checked-in authored fixtures
and loads it back.

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
