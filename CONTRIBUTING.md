# Contributing

Thanks for considering a contribution. This document covers the legal terms
under which contributions are accepted.

## License of contributions (inbound = outbound)

This project is licensed under AGPL-3.0-only. See [LICENSE](LICENSE) for the
full license text.

Contributions you submit are accepted under the same license that already
applies to the file(s) you change - "inbound = outbound". You retain
copyright on your contribution; you grant the project and its users a
license to use your contribution under the file's declared license.

## Developer Certificate of Origin (DCO)

This project uses the [Developer Certificate of Origin](https://developercertificate.org/)
(DCO) to confirm that you have the right to submit each contribution under
the terms above. Every commit must include a `Signed-off-by` trailer
matching the author of the commit:

```
Signed-off-by: Your Name <your.email@example.com>
```

Add it automatically with `git commit -s`.

## Commit messages

Commit messages must follow
[Conventional Commits](https://www.conventionalcommits.org/).
The pull request title follows them too: it is what the release automation reads
when planning versions for the packages changed by the pull request.

### Wire-touching changes carry the breaking marker

Before 1.0, the minor version is the breaking axis for the package that owns a
contract, because SemVer gives a 0.x release no major axis to break on.
So any change to a wire contract - an endpoint, a schema-tagged document, an
out-of-body envelope, an exact wire constant, or a launch contract - must carry
the breaking marker (`feat!:`, `refactor!:`), including a purely additive one.
Each package has its own compatibility identity, and supported interoperability
follows the contract it owns rather than a workspace-wide version.
A plain `feat:` or `fix:` prepares a patch release for the changed package, and
the release PR updates only packages affected by the dependency graph.

Run the owning package's contract checks before pushing a change.
Workspace architecture and code conventions are reviewed in each pull request
rather than gated by a separate policy runner.

### The toolchain floor is a compatibility promise

`rust-version` in the workspace is the floor every package in a clean checkout
builds on.
Raising it breaks builds on older toolchains without touching any wire, so the
change needs an explicit release for each affected package and a deliberate CI
review.

### The Runtime authoring surface is a compatibility promise

Runtime authoring includes the `#[phoxal::runtime]` contract, typed `Inputs` and `Outputs`, generated owner port constants, and `step`.
It is a source-level contract with every robot project that `cargo-semver-checks` cannot see because proc-macro grammar is invisible to it.
Its gate is the trybuild pass fixtures and the official services and components in this workspace: representative authored code that must keep compiling.
Grammar evolution follows the same rule as wire contracts: additions are ordinary, and a change that breaks existing authored code carries the breaking marker on the package that owns the authoring surface.

## Getting started

Open an issue or draft PR early for non-trivial changes - alignment before
code is cheaper than alignment after code.
