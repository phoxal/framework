# phoxal

The one Phoxal framework library: the participant SDK, the wire-contract
families, the typed bus, the canonical robot model, and the runtime bundle.

## Consumer profiles

One crate, one train, one compatibility identity - and one Cargo feature per
supported consumer role, so a robot project is not handed the surfaces the
processes *around* a robot use.

```toml
# A robot project. The default is deliberately participant-first.
phoxal = "0.66"

# An application attaching to a running execution, plus authored-source tooling.
phoxal = { version = "0.66", default-features = false, features = ["session", "authoring"] }

# An external simulator adapter.
phoxal = { version = "0.66", default-features = false, features = ["simulator"] }
```

`supervisor` is the framework-owned `phoxal-supervisor` executable's own
profile and takes the exact train. `test-harness` adds explicit participant
test support as a dev-dependency feature.

Profiles are additive compilation and visibility controls, never authority
boundaries: Cargo unifies features, and who may do what at runtime remains
process ownership and the constructible API. docs.rs builds every profile, so
each host item there carries the feature it needs.

Use the API documentation on <https://docs.rs/phoxal> and the public Phoxal
documentation at <https://phoxal.com>. The source is intentionally the
authoritative place for implementation details and compile-time contracts.

## License

AGPL-3.0-only. A commercial license is available; see the repository root.
