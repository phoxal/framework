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

## Build prerequisites

Building this workspace requires Webots R2025a.
The Webots controller crate links `libController` at build time, so this
applies to `cargo check` and `cargo clippy` too, not only `cargo test` or a
release build, since build scripts run on check.
Install Webots R2025a and either use its default install location or set
`WEBOTS_HOME` to point at it.

## Getting started

Open an issue or draft PR early for non-trivial changes - alignment before
code is cheaper than alignment after code.
