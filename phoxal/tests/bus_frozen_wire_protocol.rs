//! The frozen bootstrap fact `phoxal::bus` declares, held against the
//! transport it links.

use std::fs;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;

/// The workspace this crate is part of: the parent of its own directory, since
/// the framework library sits at `phoxal/` under the workspace root.
fn workspace_root() -> Result<PathBuf> {
    Ok(Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(1)
        .context("this crate's manifest directory has no workspace root")?
        .to_path_buf())
}

/// The Zenoh wire protocol version `phoxal::bus` freezes is the one the linked
/// transport actually speaks.
///
/// The bus declares the version in its own contract surface, because the
/// compatibility checker reads a declared boundary and `zenoh-protocol` is an
/// internal crate of the transport that no Phoxal crate depends on. This is
/// what stops that declaration going stale behind a Zenoh upgrade: the resolved
/// `zenoh-protocol` is read from the source Cargo actually compiles, and its
/// own constant is compared against what the bus declares.
///
/// It lives here rather than in the `cargo xtask policy` gate because only this
/// crate can state what it declares: reading `__compat::contract_surface()`
/// means linking the bus, and the runner deliberately links no framework crate.
///
/// A Zenoh release that moves this number is a bootstrap-breaking event and not
/// a routine dependency bump: peers that disagree on it never form a session,
/// so nothing above it gets the chance to report the disagreement. See
/// xtask/README.md "When a gate fails", rule 3 "A frozen bootstrap fact
/// drifted".
#[test]
fn the_frozen_zenoh_wire_protocol_version_is_the_one_the_transport_speaks() -> Result<()> {
    let metadata = MetadataCommand::new()
        .manifest_path(workspace_root()?.join("Cargo.toml"))
        .exec()
        .context("workspace cargo metadata failed")?;
    let resolved = metadata
        .packages
        .iter()
        .filter(|package| package.name.as_str() == "zenoh-protocol")
        .collect::<Vec<_>>();
    assert_eq!(
        resolved.len(),
        1,
        "the workspace must link exactly one zenoh-protocol, or the version read here is not the \
         one the bus speaks: {:?}",
        resolved
            .iter()
            .map(|package| package.version.to_string())
            .collect::<Vec<_>>()
    );
    let source_root = resolved[0]
        .manifest_path
        .parent()
        .context("the resolved zenoh-protocol has no source directory")?;
    let source = fs::read_to_string(source_root.join("src/lib.rs").as_std_path())
        .context("the resolved zenoh-protocol source could not be read")?;

    let literal = source
        .lines()
        .find_map(|line| {
            line.trim()
                .strip_prefix("pub const VERSION: u8 = ")?
                .strip_suffix(';')
        })
        .context(
            "zenoh-protocol no longer states its wire protocol version as `pub const VERSION: \
             u8`. That is itself a transport change worth reading before it ships: find where the \
             version now lives, then follow xtask/README.md \"When a gate fails\", rule 3.",
        )?;
    let spoken = match literal.trim().strip_prefix("0x") {
        Some(hexadecimal) => u8::from_str_radix(hexadecimal, 16),
        None => literal.trim().parse::<u8>(),
    }
    .with_context(|| format!("zenoh-protocol states an unreadable wire version `{literal}`"))?;

    let declared =
        format!(r#"{{"name":"zenoh-wire-protocol","record":"identifier","value":"{spoken}"}}"#);
    let surface = phoxal::bus::__compat::contract_surface();
    assert!(
        surface.contains(&declared),
        "phoxal::bus freezes a Zenoh wire protocol version the linked transport does not speak. \
         The transport says {spoken}. This is a bootstrap-breaking transport change: do not edit \
         the pin to match. See xtask/README.md \"When a gate fails\", rule 3.\n{surface}"
    );
    Ok(())
}
