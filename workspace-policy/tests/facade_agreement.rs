//! Where the authoring crate and the runtime crate must agree.
//!
//! `phoxal-manifest` authors documents and `phoxal` consumes them, and the two
//! do not depend on each other. Nothing inside either crate can prove they
//! still describe the same grammar, so this crate - which depends on both -
//! feeds one what the other declares.

use anyhow::{Context, Result};

/// The declared document identities and `phoxal-manifest`'s own
/// `#[serde(tag = "schema")]` variants are one contract owned by two crates.
/// A serde rename takes a literal, so the only way to pin the two together is
/// to feed the manifest compiler a real document whose tag is written from the
/// identity enum.
#[test]
fn the_facade_manifest_schemas_match_the_documents_phoxal_manifest_accepts() -> Result<()> {
    use phoxal::__private::compatibility as compat;

    // Serialized, not `as_str`: what a peer writes on the wire is what
    // `phoxal-manifest` has to accept.
    fn tag(value: impl serde::Serialize) -> Result<String> {
        Ok(serde_json::from_value::<String>(serde_json::to_value(
            value,
        )?)?)
    }

    phoxal_manifest::source::robot::Manifest::parse(&format!(
        r#"schema: {}
robot:
  id: rover
  namespace: dev
  kinematic: {{ kind: omnidirectional, actuators: [drive.motor], encoders: [] }}
  motion_limits: {{ max_linear_speed_mps: 0.5, max_angular_speed_radps: 1.0 }}
  components:
    drive:
      component: drive
      mount_link: base_link
"#,
        tag(compat::ROBOT)?
    ))
    .context("phoxal-manifest must accept the robot grammar the facade declares")?;
    phoxal_manifest::source::component::Manifest::parse(&format!(
        "schema: {}\ncapabilities: {{}}\n",
        tag(compat::COMPONENT)?
    ))
    .context("phoxal-manifest must accept the component grammar the facade declares")?;
    phoxal_manifest::source::simulation::Manifest::parse(&format!(
        "schema: {}\ncapabilities: {{}}\n",
        tag(compat::SIMULATION)?
    ))
    .context("phoxal-manifest must accept the simulation grammar the facade declares")?;
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
