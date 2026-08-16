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
to size the next train.

### Wire-touching changes carry the breaking marker

Before 1.0 the minor *is* the compatibility line, because SemVer gives a 0.x
release no major axis to break on.
So any change to a wire contract - an endpoint, a schema-tagged document, an
out-of-body envelope, an exact wire constant, a launch contract - must carry the
breaking marker (`feat!:`, `refactor!:`), a purely additive one included.
The train version is the single compatibility identity peers match on, so an
endpoint that exists in only one of two trains has to move that line just as a
removed one does.
A plain `feat:` or `fix:` prepares a patch train, and the release PR's
compatibility gate fails a candidate version too small for what its contracts
changed.

Run `cargo xtask compatibility report` to see the impact of a change before
pushing it; every pull request runs the same report.

### The toolchain floor is a compatibility promise

`rust-version` in the workspace is the floor every robot project builds on.
Raising it breaks builds on older toolchains without touching any wire, so a
raise is never a patch: before 1.0 it needs the next line, and from 1.0 on it
is a deliberate minor.
The release gate reads the published train's floor from the registry index and
refuses an under-sized candidate; acknowledge a deliberate raise with the
gate's flag and size the release accordingly.

### The authoring surface is a compatibility promise

Participant authoring - the role attributes, the fragment grammar, `step`, the
`phoxal::api` facade - is a source-level contract with every robot project that
`cargo-semver-checks` cannot see, because proc-macro grammar is invisible to
it.
Its gate is the example build (`examples/hello-rover` in CI) plus the trybuild
pass fixtures: representative authored code that must keep compiling.
Grammar evolution follows the same rule as wire contracts: additions are
ordinary, and a change that breaks existing authored code carries the breaking
marker.

## Getting started

Open an issue or draft PR early for non-trivial changes - alignment before
code is cheaper than alignment after code.
