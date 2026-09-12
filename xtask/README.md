# xtask

The workspace's command runner is reached as `cargo xtask <verb>` through the
alias in [`.cargo/config.toml`](../.cargo/config.toml).

```sh
cargo xtask policy
```

## Workspace policy

`cargo xtask policy` checks facts that no individual crate owns:

- published library and executable package layout;
- internal dependency direction and forbidden runtime edges;
- explicit feature and consumer-profile boundaries;
- registry publication and independent release-plz configuration;
- retained transport and test-module ownership rules; and
- deleted surfaces and forbidden issue or decision references.

The policy command reads Cargo metadata, source files, configuration, and Git
without linking the framework or executing a published package.

Cargo owns package resolution and package versions.
Buf owns Protobuf schema checks and each service owns its generated contract.
The runner does not compare packages against a synchronized framework train,
infer a release size, or maintain a second compatibility baseline.
