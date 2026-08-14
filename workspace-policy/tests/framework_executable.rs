//! The framework-owned supervisor's workspace and release integration.

use std::collections::BTreeSet;
use std::fs;

use anyhow::{Context, Result};
use cargo_metadata::MetadataCommand;
use workspace_policy::artifact::ArtifactKind;
use workspace_policy::framework_executable::SPECS;
use workspace_policy::registry::Workspace;
use workspace_policy::workspace_root;

#[test]
fn supervisor_is_a_default_workspace_member_and_non_catalog_registry_executable() -> Result<()> {
    let root = workspace_root()?;
    let metadata = MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("read framework workspace metadata")?;
    let supervisor = metadata
        .workspace_packages()
        .into_iter()
        .find(|package| package.name.as_str() == "phoxal-supervisor")
        .context("phoxal-supervisor is absent from workspace members")?;

    assert!(metadata.workspace_members.contains(&supervisor.id));
    assert!(
        metadata.workspace_default_members.contains(&supervisor.id),
        "plain root cargo commands must build the supervisor"
    );
    assert!(
        supervisor
            .publish
            .as_deref()
            .is_some_and(|publish| publish == ["phoxal"])
    );
    assert_eq!(
        supervisor.manifest_path.as_std_path().strip_prefix(root)?,
        std::path::Path::new("supervisor/Cargo.toml")
    );
    assert!(
        supervisor
            .dependencies
            .iter()
            .all(|dependency| dependency.name.as_str() != "phoxal"),
        "the supervisor must not enter the participant graph through the authoring facade"
    );

    let policy =
        Workspace::discover(MetadataCommand::new().manifest_path(root.join("Cargo.toml")))?;
    let [framework_executable] = policy.framework_executables() else {
        anyhow::bail!("the workspace must contain exactly one framework-owned executable");
    };
    assert_eq!(framework_executable.spec, SPECS[0]);
    assert!(
        policy
            .official_artifacts()
            .iter()
            .all(|artifact| artifact.package_name() != "phoxal-supervisor")
    );
    assert!(ArtifactKind::try_from("supervisor").is_err());
    Ok(())
}

#[test]
fn every_registry_executable_participates_in_the_release_train() -> Result<()> {
    let root = workspace_root()?;
    let metadata = MetadataCommand::new()
        .manifest_path(root.join("Cargo.toml"))
        .no_deps()
        .exec()
        .context("read framework workspace metadata")?;
    let registry_packages = metadata
        .workspace_packages()
        .into_iter()
        .filter(|package| {
            package
                .publish
                .as_deref()
                .is_some_and(|publish| publish == ["phoxal"])
        })
        .map(|package| package.name.to_string())
        .collect::<BTreeSet<_>>();
    assert!(registry_packages.contains("phoxal-supervisor"));

    let release_source = fs::read_to_string(root.join("release-plz.toml"))?;
    let release = release_source
        .parse::<toml_edit::DocumentMut>()
        .context("release-plz.toml is invalid")?;
    let packages = release["package"]
        .as_array_of_tables()
        .context("release-plz.toml has no package list")?;
    let mut configured_registry_packages = BTreeSet::new();
    for package in packages {
        let Some(name) = package.get("name").and_then(toml_edit::Item::as_str) else {
            continue;
        };
        if !registry_packages.contains(name) {
            continue;
        }
        assert_eq!(
            package
                .get("version_group")
                .and_then(toml_edit::Item::as_str),
            Some("framework-train"),
            "{name} must move with the framework train"
        );
        for flag in [
            "publish",
            "changelog_update",
            "git_tag_enable",
            "git_release_enable",
        ] {
            assert_eq!(
                package.get(flag).and_then(toml_edit::Item::as_bool),
                Some(false),
                "{name} must set {flag} = false"
            );
        }
        assert!(
            package.get("release").is_none(),
            "{name} must participate in release-plz change detection"
        );
        configured_registry_packages.insert(name.to_owned());
    }
    assert_eq!(configured_registry_packages, registry_packages);

    let workflow = fs::read_to_string(root.join(".github/workflows/release-plz.yml"))?;
    assert!(
        workflow.matches("select(.publish == [\"phoxal\"])").count() >= 4,
        "registry packaging and verification must remain metadata-driven"
    );
    assert!(
        !workflow.contains("cargo package -p phoxal-supervisor"),
        "the supervisor must use the neutral executable batch"
    );
    Ok(())
}
