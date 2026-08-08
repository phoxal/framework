//! Where the runtime facade and process-contract crate must agree.

use anyhow::Result;

/// The runtime compatibility identities are one contract owned by the facade
/// and the embedded participant metadata writer. Keeping this assertion here
/// prevents a facade constant from drifting away from its wire enum.
#[test]
fn the_facade_runtime_schemas_match_the_process_contract() -> Result<()> {
    use phoxal::__private::compatibility as compat;
    use phoxal_runtime_contract::metadata::ParticipantSchemas;
    use phoxal_runtime_contract::version::{BusAbi, LaunchAbi, RuntimeSchema};

    // Serialized, not `as_str`: what a peer writes on the wire is what
    // `phoxal-manifest` has to accept.
    fn tag(value: impl serde::Serialize) -> Result<String> {
        Ok(serde_json::from_value::<String>(serde_json::to_value(
            value,
        )?)?)
    }

    let schemas = ParticipantSchemas {
        bus: compat::BUS,
        launch: compat::LAUNCH,
        runtime: compat::RUNTIME,
    };
    assert_eq!(schemas.bus, BusAbi::V0);
    assert_eq!(schemas.launch, LaunchAbi::V0);
    assert_eq!(schemas.runtime, RuntimeSchema::V0);
    assert_eq!(tag(compat::BUS)?, compat::BUS.as_str());
    assert_eq!(tag(compat::LAUNCH)?, compat::LAUNCH.as_str());
    assert_eq!(tag(compat::RUNTIME)?, compat::RUNTIME.as_str());
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
