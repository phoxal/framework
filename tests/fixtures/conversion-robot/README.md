# Conversion qualification robot

This robot executes ordinary Rust conversions inside the canonical brain runtime.
Its process tests exercise source capture provenance, freshness, real conversion failure, and reset behavior with World and Navigation.
Prepare and build its bundle before starting Cargo tests so the test process never recursively acquires its own Cargo build lock.

```sh
cargo phoxal prepare
cargo phoxal build
PHOXAL_CONVERSION_BUNDLE=/absolute/path/to/runnable/build cargo test --features host-acceptance --test composition -- --ignored --nocapture
```

Linux and macOS are supported.
Windows is unsupported.

Use the runnable directory reported by the build command, not its ZIP archive.
The `host-acceptance` feature keeps the explicit process qualification separate from ordinary unit tests.
