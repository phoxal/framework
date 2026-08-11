//! Where the runtime facade and process-contract crate must agree.

use anyhow::Result;

/// Compatibility is one identity - the framework train version - reached
/// through three spellings that must all be the same fact: the facade constant
/// a role macro splices into a binary, the process-contract value a running
/// participant reads its compatibility line from, and the workspace version
/// Cargo builds everything from. All three are the exact train; only the
/// comparison between two of them is the line.
#[test]
fn the_facade_framework_version_is_the_workspace_train_version() -> Result<()> {
    use phoxal::__private::compatibility as compat;
    use phoxal_runtime_contract::version::FrameworkVersion;

    // Serialized, not `Display`: what a peer writes on the wire is what a
    // reader has to accept.
    fn tag(value: impl serde::Serialize) -> Result<String> {
        Ok(serde_json::from_value::<String>(serde_json::to_value(
            value,
        )?)?)
    }

    assert_eq!(compat::FRAMEWORK, FrameworkVersion::CURRENT.to_string());
    assert_eq!(tag(FrameworkVersion::CURRENT)?, compat::FRAMEWORK);
    // Every workspace member inherits `[workspace.package] version`, so this
    // crate's own package version is the train version.
    assert_eq!(compat::FRAMEWORK, env!("CARGO_PKG_VERSION"));
    Ok(())
}

/// An asset id is written by the manifest compiler and read by the runtime, so
/// the two must accept and reject exactly the same paths.
#[test]
fn compiled_asset_id_rules_match_at_the_producer_and_consumer_boundary() {
    let cases = [
        ("meshes/base.stl", true),
        ("components/camera/config.json", true),
        ("", false),
        ("/absolute", false),
        ("../secret", false),
        ("a/../b", false),
        ("a\\b", false),
        ("a//b", false),
        ("a/./b", false),
    ];
    for (value, accepted) in cases {
        assert_eq!(
            phoxal_model::AssetId::new(value).is_ok(),
            accepted,
            "manifest producer disagreed for {value:?}"
        );
        assert_eq!(
            phoxal::AssetId::new(value).is_ok(),
            accepted,
            "runtime consumer disagreed for {value:?}"
        );
    }
}
