# phoxal-manifest

Compiler for authored Phoxal project manifests. It parses project source files,
validates and resolves them, and produces one canonical `phoxal-model` robot plus
the deterministic assets that go beside it, for build tooling such as
`phoxal-cli`.

Everything the compiled robot says is in that one value: its `services` with
their configuration, its `components` with their driver blocks, and its
`component_types` with each type's simulation folded in. There is no second list
of services or drivers to keep in step with it.

`SourceSet::compile` takes the official service set as an argument. Which
services are official is a build-tooling fact that changes with the packages the
CLI resolves, so the framework holds no list of its own; the caller's set is
merged with the authored `services:` map, and an authored entry wins.

Where the compiled robot is written - `manifest.json` beside `assets/` and
`bin/` - is owned by the sibling `phoxal-bundle` crate. Runtime processes do not
depend on this crate.

## Authored documents

`source` holds the exact serde shape of each authored document: `robot.yaml`,
`component.yaml` and `simulation.yaml`.
A `schema:` tag names the source language a document is written in; it is a generation of an authored grammar and never a framework compatibility identity, which stays `FrameworkVersion` alone.
Each kind owns its generation independently, so `robot.yaml` v1 can be introduced without forcing the other two to advance.

A generation's syntax ends at one boundary.
Every versioned DTO normalizes into the crate-internal `normalized` form, and the compiler reads only that, so a new generation is a new DTO plus a new `normalize` - never a second copy of the compiler, and never a change to anything this crate or `phoxal-model` exposes.
A test-only second source language proves that end to end: it normalizes into the same value and compiles to the same canonical output as the equivalent current document.

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

## What a release owes an author

This crate is not a wire surface - no two binaries negotiate over a `robot.yaml` -
so the contract-surface comparison cannot see a grammar change here at all.
The promise it does carry is directional: a newer compatible framework keeps
reading every document an older compatible framework accepted, with the same
meaning, and is free to accept more.

`cargo xtask compatibility report` gates that. It compiles a corpus of the
repository's authored projects through both this reader and the published one,
and calls a document that stopped compiling, or that compiles to a different
canonical model, source-breaking. The remedy lives in the versioned DTO's
`normalize`, which is the only place a generation's syntax and defaults are
owned; see `xtask/README.md` rule 8.
