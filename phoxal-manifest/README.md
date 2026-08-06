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

`schema::generate` generates Draft 2020-12 editor schemas from the exact authored
serde DTOs. They provide portable YAML completion and inspection only;
`phoxal validate` remains authoritative for strict YAML, semantic, cross-file,
and project-resolution validation.
