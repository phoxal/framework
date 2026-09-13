//! The official artifact package grammar: where an artifact package lives,
//! what it is called, where it publishes, and which targets it may carry.
//!
//! One artifact is one Cargo package at `{services,components}/<id>`,
//! producing the exact target shape for its kind. Services and components each
//! expose one reusable implementation library and one package-named binary.
//! Components also carry their `component.yaml`, canonical `model.xml`, and
//! any locally referenced assets. Discovery reads the workspace metadata and
//! rejects any package that claims to be an artifact without obeying the
//! grammar, which is what keeps the directory, crate name, and published
//! identity from drifting apart.
//!
//! A directory holds many artifacts and reads plural; a name qualifies one and
//! reads singular, so `services/drive` is the crate `phoxal-service-drive`.
//! [`ArtifactKind`] owns both spellings and is the only place that knows they
//! differ.

use std::collections::BTreeSet;
use std::fmt;
use std::path::{Component, Path};

use anyhow::{Context, Result, bail};
use cargo_metadata::Target;
use serde_json::Value;

use super::executable::PHOXAL_PROVIDER;
use super::executable::{ServiceTargetSpec, validate_registry_publish, validate_service_targets};
use super::{
    FACADE, LIBRARY_CRATE_ROOT, Subject, Violation, is_internal_package_directory,
    is_library_directory, library_package_name,
};

/// The Cargo `package.name` prefix backing [`PHOXAL_PROVIDER`]: the package
/// `phoxal/service-drive` is the crate `phoxal-service-drive`. The two
/// spellings of the provider are pinned to each other by
/// `the_crate_prefix_spells_the_provider`.
const PHOXAL_PACKAGE_PREFIX: &str = "phoxal-";

/// The kind of an official artifact. Each kind renders two ways: as the
/// top-level directory its packages live under ([`ArtifactKind::directory`])
/// and as the leading segment of their names ([`ArtifactKind::name_segment`]).
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ArtifactKind {
    Service,
    /// A component crate: one reusable Runtime implementation library, its
    /// package-named driver binary, and its authored assets (`component.yaml`,
    /// `model.xml`, and any locally referenced files) in one package.
    Component,
}

impl ArtifactKind {
    /// Every kind, in the order discovery reports artifacts.
    pub const ALL: [Self; 2] = [Self::Service, Self::Component];

    /// The top-level directory this kind's packages live under.
    ///
    /// This is a repository-layout fact and nothing else. It is deliberately
    /// *not* the leading segment of the package names, which
    /// [`Self::name_segment`] owns: the directory holds many artifacts and so
    /// reads plural, while a name segment qualifies exactly one and so reads
    /// singular. The two are pinned to each other by
    /// `the_directory_and_name_segment_of_every_kind_are_pinned`, which is
    /// what keeps a rename of either from silently moving the other.
    pub fn directory(self) -> &'static str {
        match self {
            Self::Service => "services",
            Self::Component => "components",
        }
    }

    /// The leading segment of both of this kind's names: `service` in
    /// `phoxal/service-drive` and in `phoxal-service-drive`.
    ///
    /// This is published identity. It reaches the `phoxal` registry and every
    /// robot manifest that names an artifact, so it is
    /// frozen independently of where the sources happen to sit in this
    /// repository.
    pub fn name_segment(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Component => "component",
        }
    }

    /// The provider-qualified public identity for an artifact of this kind
    /// (`phoxal/service-drive`, `phoxal/component-ddsm115`, ...). There is no
    /// `kind` suffix: a component crate is one package carrying both its
    /// binary and its asset outputs.
    pub fn package_identity(self, id: &ArtifactId) -> String {
        format!("{PHOXAL_PROVIDER}/{}", self.package_segment(id))
    }

    /// The Cargo `package.name` a crate-backed artifact of this kind must use.
    pub fn package_name(self, id: &ArtifactId) -> String {
        format!("{PHOXAL_PACKAGE_PREFIX}{}", self.package_segment(id))
    }

    /// The kind and id a Cargo package name claims by its prefix. A package
    /// whose name claims an artifact kind but whose manifest sits outside the
    /// directory grammar is a violation, not a non-artifact, and this is how
    /// discovery tells the two apart.
    fn from_package_name(package_name: &str) -> Option<(Self, ArtifactId)> {
        let tail = package_name.strip_prefix(PHOXAL_PACKAGE_PREFIX)?;
        Self::ALL.into_iter().find_map(|kind| {
            let id = tail.strip_prefix(kind.name_segment())?.strip_prefix('-')?;
            Some((kind, ArtifactId::new(id).ok()?))
        })
    }

    /// The shared tail of both names: `service-drive` in `phoxal/service-drive`
    /// and in `phoxal-service-drive`.
    fn package_segment(self, id: &ArtifactId) -> String {
        format!("{}-{id}", self.name_segment())
    }

    /// A package's name is a function of the kind whose directory it lives in,
    /// so a mismatch means one of the two moved without the other.
    fn validate_package_name(
        self,
        package_name: &str,
        id: &ArtifactId,
        root: &Path,
        manifest_path: &Path,
    ) -> Result<()> {
        let expected = self.package_name(id);
        if package_name != expected {
            bail!(
                "{} is in artifact directory for {self} but package.name is '{package_name}'; \
                 expected '{expected}'",
                relative_display(root, manifest_path)
            );
        }
        Ok(())
    }
}

impl fmt::Display for ArtifactKind {
    /// Renders the kind as its directory segment, the same token
    /// [`ArtifactKind::try_from`] parses.
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(self.directory())
    }
}

impl TryFrom<&str> for ArtifactKind {
    type Error = UnknownArtifactKind;

    /// The inverse of [`fmt::Display`]: parses a top-level directory segment
    /// back into the kind that owns it.
    fn try_from(directory: &str) -> Result<Self, Self::Error> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.directory() == directory)
            .ok_or_else(|| UnknownArtifactKind(directory.to_owned()))
    }
}

/// A directory segment that names no artifact kind.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct UnknownArtifactKind(String);

impl fmt::Display for UnknownArtifactKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "'{}' names no artifact kind; expected one of {}",
            self.0,
            ArtifactKind::ALL.map(ArtifactKind::directory).join(", ")
        )
    }
}

impl std::error::Error for UnknownArtifactKind {}

/// The `<id>` of an official artifact: the directory it occupies under its
/// kind, and the tail of both of its package names.
#[derive(Clone, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub struct ArtifactId(String);

impl ArtifactId {
    /// An empty id would render as `phoxal/service-`, a package identity with
    /// no package in it, so the type refuses to hold one.
    pub fn new(value: &str) -> Result<Self> {
        if value.is_empty() {
            bail!("an artifact id must not be empty");
        }
        Ok(Self(value.to_owned()))
    }
}

impl fmt::Display for ArtifactId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.pad(&self.0)
    }
}

/// One discovered official artifact package.
///
/// The kind and the id are the identity; every name this artifact answers to
/// is rendered from them rather than stored, so no two renderings can drift.
#[derive(Clone, Debug)]
pub struct OfficialArtifact {
    pub kind: ArtifactKind,
    pub id: ArtifactId,
}

impl OfficialArtifact {
    /// The provider-qualified public identity, e.g. `phoxal/component-ddsm115`.
    /// This is the sole public identity; asset filenames are filesystem-safe
    /// projections of it.
    pub fn package(&self) -> String {
        self.kind.package_identity(&self.id)
    }

    /// The Cargo crate name backing this package, e.g.
    /// `phoxal-component-ddsm115`.
    pub fn package_name(&self) -> String {
        self.kind.package_name(&self.id)
    }

    /// Every official executable publishes to the `phoxal` registry and only
    /// there.
    ///
    /// This is the Cargo-side guard against an accidental crates.io
    /// publication. It also keeps the package visible to release-plz's
    /// change-driven version planner.
    fn validate_publish(
        package_name: &str,
        publish: Option<&[String]>,
        root: &Path,
        manifest_path: &Path,
    ) -> Result<()> {
        validate_registry_publish(
            package_name,
            "an official artifact package",
            publish,
            root,
            manifest_path,
        )
    }

    /// Enforces the target convention for an official artifact package.
    ///
    /// Services and components publish one reusable implementation library
    /// alongside their package-named process binary.
    fn validate_targets(
        kind: ArtifactKind,
        package_name: &str,
        targets: &[Target],
        root: &Path,
        manifest_path: &Path,
    ) -> Result<()> {
        let package_directory = manifest_path
            .parent()
            .context("official artifact manifest has no parent")?;
        let package_relative = package_directory
            .strip_prefix(root)
            .unwrap_or(package_directory);
        let relative_source = |name: &str| package_relative.join("src").join(name);
        match kind {
            ArtifactKind::Service => {
                let expected_lib = package_name.replace('-', "_");
                let expected_bin_source = relative_source("main.rs");
                let expected_lib_source = relative_source("lib.rs");
                validate_service_targets(
                    package_name,
                    "an official service package",
                    ServiceTargetSpec {
                        expected_bin: package_name,
                        expected_lib: &expected_lib,
                        expected_bin_source: Some(&expected_bin_source),
                        expected_lib_source: Some(&expected_lib_source),
                    },
                    targets,
                    root,
                )
            }
            ArtifactKind::Component => {
                let expected_lib = package_name.replace('-', "_");
                let expected_bin_source = relative_source("main.rs");
                let expected_lib_source = relative_source("lib.rs");
                validate_service_targets(
                    package_name,
                    "an official component package",
                    ServiceTargetSpec {
                        expected_bin: package_name,
                        expected_lib: &expected_lib,
                        expected_bin_source: Some(&expected_bin_source),
                        expected_lib_source: Some(&expected_lib_source),
                    },
                    targets,
                    root,
                )
            }
        }
    }
}

/// Official artifacts carry their publication role in Cargo metadata as well
/// as in their repository path. The two declarations are intentionally checked
/// together so registry admission cannot silently treat an artifact as another
/// package role when its manifest is copied or republished.
fn validate_role_metadata(
    kind: ArtifactKind,
    package_name: &str,
    metadata: &Value,
    root: &Path,
    manifest_path: &Path,
) -> Result<()> {
    let actual = metadata
        .get("phoxal")
        .and_then(Value::as_object)
        .and_then(|phoxal| phoxal.get("kind"))
        .and_then(Value::as_str);
    if actual != Some(kind.name_segment()) {
        let found = actual.map_or_else(
            || "missing or non-string kind".to_owned(),
            |value| format!("kind = {value:?}"),
        );
        bail!(
            "{package_name} is an official {kind} package but {} must declare exactly \
             [package.metadata.phoxal] kind = \"{}\"; found {found}",
            relative_display(root, manifest_path),
            kind.name_segment()
        );
    }
    Ok(())
}

pub(crate) fn discover_package(
    root: &Path,
    package: &cargo_metadata::Package,
) -> Result<Option<OfficialArtifact>> {
    let package_name = package.name.to_string();
    let manifest_path = package.manifest_path.clone().into_std_path_buf();
    let manifest = ManifestClassification::classify(root, &manifest_path)
        .with_context(|| format!("failed to classify {}", manifest_path.display()))?;
    let ManifestClassification::Artifact { kind, id } = manifest else {
        if let Some((prefix_kind, prefix_id)) = ArtifactKind::from_package_name(&package_name) {
            bail!(
                "{package_name} uses the {prefix_kind} artifact package prefix with id \
                 '{prefix_id}' but its manifest path {} is outside the exact directory grammar \
                 for that kind",
                relative_display(root, &manifest_path)
            );
        }
        return Ok(None);
    };

    kind.validate_package_name(&package_name, &id, root, &manifest_path)?;
    validate_role_metadata(kind, &package_name, &package.metadata, root, &manifest_path)?;
    OfficialArtifact::validate_publish(
        &package_name,
        package.publish.as_deref(),
        root,
        &manifest_path,
    )?;
    OfficialArtifact::validate_targets(
        kind,
        &package_name,
        &package.targets,
        root,
        &manifest_path,
    )?;
    Ok(Some(OfficialArtifact { kind, id }))
}

/// What a workspace member's manifest path says the package is.
#[derive(Debug, Eq, PartialEq)]
enum ManifestClassification {
    /// An official artifact package, at the exact path its kind and id require.
    Artifact { kind: ArtifactKind, id: ArtifactId },
    /// A path the grammar deliberately says nothing about: a listed library
    /// crate, published or internal, or this policy crate.
    Excluded,
    /// Any other workspace member. Discovery skips it exactly as it skips
    /// `Excluded`; the two stay distinct so a test can tell an intended
    /// exclusion from a path the grammar simply does not recognize.
    NonArtifact,
}

impl ManifestClassification {
    fn classify(root: &Path, manifest_path: &Path) -> Result<Self> {
        let relative = manifest_path.strip_prefix(root).with_context(|| {
            format!(
                "{} is not under workspace root {}",
                manifest_path.display(),
                root.display()
            )
        })?;
        let components = path_components(relative)?;
        let Some(&top_level) = components.first() else {
            bail!("manifest path {} is empty", relative.display());
        };

        let [directory @ .., "Cargo.toml"] = components.as_slice() else {
            bail!(
                "workspace package manifest {} is not named Cargo.toml",
                relative.display()
            );
        };
        let directory = directory.join("/");
        if is_library_directory(&directory) || is_internal_package_directory(&directory) {
            return Ok(Self::Excluded);
        }
        // A manifest under the library root that neither list names is a
        // violation rather than a package the grammar says nothing about:
        // `crates/` is reserved for the workspace's own libraries, and a
        // package sitting there unlisted would be skipped by every rule in
        // this crate.
        if top_level == LIBRARY_CRATE_ROOT {
            bail!(
                "workspace package manifest {} is under '{LIBRARY_CRATE_ROOT}/' but is not a \
                 listed library crate; a library crate is the package '{}' at \
                 '{directory}', and must be added to LIBRARY_CRATE_DIRS or \
                 INTERNAL_CRATE_DIRS",
                relative.display(),
                library_package_name(&directory).unwrap_or_else(|| format!(
                    "{FACADE}-<suffix>' at '{LIBRARY_CRATE_ROOT}/<suffix>"
                ))
            );
        }

        let Ok(kind) = ArtifactKind::try_from(top_level) else {
            return Ok(Self::NonArtifact);
        };
        let [_, id, "Cargo.toml"] = components.as_slice() else {
            bail!(
                "workspace package manifest {} is nested under artifact root '{top_level}'; \
                 official artifacts must live exactly at {{{}}}/<id>/Cargo.toml",
                relative.display(),
                ArtifactKind::ALL.map(ArtifactKind::directory).join(",")
            );
        };

        Ok(Self::Artifact {
            kind,
            id: ArtifactId::new(id)?,
        })
    }
}

fn path_components(path: &Path) -> Result<Vec<&str>> {
    path.components()
        .map(|component| match component {
            Component::Normal(value) => value
                .to_str()
                .with_context(|| format!("path component in {} is not UTF-8", path.display())),
            _ => bail!("path {} contains non-normal component", path.display()),
        })
        .collect()
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

/// The exact set of official artifacts this workspace releases.
///
/// Discovery has already rejected anything that claims to be an artifact
/// without obeying the grammar, so what is left to state is the scope itself:
/// which official artifact packages exist. Spelled out in full rather than
/// counted, because a package silently entering or leaving the release scope
/// is the failure this rule exists to catch.
const OFFICIAL_ARTIFACT_RELEASE_SCOPE: [&str; 10] = [
    "phoxal/component-bno085",
    "phoxal/component-ddsm115",
    "phoxal/component-oak_d_lite",
    "phoxal/component-vl53l1x",
    "phoxal/component-zed_f9p",
    "phoxal/service-kinematics",
    "phoxal/service-motion",
    "phoxal/service-navigation",
    "phoxal/service-safety",
    "phoxal/service-world",
];

/// Whether a workspace directory is an exact official artifact package
/// directory. Artifact libraries are implementation targets, not reusable
/// library crates, so the library-directory completeness rule excludes them
/// after artifact discovery has validated their own lib+bin grammar.
pub(crate) fn is_official_artifact_directory(directory: &str) -> bool {
    let Some((kind, id)) = directory.split_once('/') else {
        return false;
    };
    matches!(kind, "services" | "components") && !id.is_empty() && !id.contains('/')
}

pub(super) fn the_official_artifact_release_scope_is_exact(
    subject: &Subject,
) -> Result<Vec<Violation>> {
    let workspace = match subject.executables() {
        Ok(workspace) => workspace,
        Err(violation) => return Ok(vec![violation]),
    };
    let discovered = workspace
        .official_artifacts()
        .iter()
        .map(OfficialArtifact::package)
        .collect::<BTreeSet<_>>();
    let expected = OFFICIAL_ARTIFACT_RELEASE_SCOPE
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();

    let mut violations = Vec::new();
    for package in discovered.difference(&expected) {
        violations.push(Violation::new(format!(
            "{package} is discovered as an official artifact but is not in the release scope"
        )));
    }
    for package in expected.difference(&discovered) {
        violations.push(Violation::new(format!(
            "{package} is in the release scope but is no longer discovered as an official artifact"
        )));
    }

    Ok(violations)
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use cargo_metadata::{MetadataCommand, TargetKind};

    use super::super::executable::{is_allowed_target_kind, is_library_target_kind};
    use super::super::registry::Workspace;

    use super::*;

    fn root() -> PathBuf {
        PathBuf::from("/repo")
    }

    fn id(value: &str) -> ArtifactId {
        ArtifactId::new(value).expect("test ids are non-empty")
    }

    fn classify(relative: &str) -> Result<ManifestClassification> {
        ManifestClassification::classify(&root(), &root().join(relative))
    }

    #[test]
    fn the_crate_prefix_spells_the_provider() {
        assert_eq!(PHOXAL_PACKAGE_PREFIX, format!("{PHOXAL_PROVIDER}-"));
    }

    /// The directory an artifact's sources sit in and the segment its names
    /// are built from are two independent facts: one is repository layout, the
    /// other is published identity. Nothing derives one from the other, so
    /// this is the only thing standing between a layout change and a silent
    /// rename of every published package. Both columns are spelled out in
    /// full, because a rule that computed one from the other would be the
    /// coupling this test exists to prevent.
    #[test]
    fn the_directory_and_name_segment_of_every_kind_are_pinned() {
        assert_eq!(
            ArtifactKind::ALL.map(|kind| (kind.directory(), kind.name_segment())),
            [("services", "service"), ("components", "component")]
        );
    }

    /// `Display` and `TryFrom` are one mapping written twice, so every variant
    /// must survive the round trip through its directory segment.
    #[test]
    fn every_kind_round_trips_through_its_directory_segment() {
        for kind in ArtifactKind::ALL {
            assert_eq!(ArtifactKind::try_from(kind.to_string().as_str()), Ok(kind));
            assert_eq!(ArtifactKind::try_from(kind.directory()), Ok(kind));
        }
        let error = ArtifactKind::try_from("library").unwrap_err();
        assert_eq!(
            error.to_string(),
            "'library' names no artifact kind; expected one of services, components"
        );
    }

    #[test]
    fn package_identity_is_provider_qualified() {
        assert_eq!(
            ArtifactKind::Service.package_identity(&id("drive")),
            "phoxal/service-drive"
        );
        assert_eq!(
            ArtifactKind::Component.package_identity(&id("ddsm115")),
            "phoxal/component-ddsm115"
        );
    }

    /// The public identity and the crate name are two renderings of one
    /// identity, and a package name parses back into the pair that rendered it.
    #[test]
    fn a_package_name_parses_back_into_its_kind_and_id() {
        for kind in ArtifactKind::ALL {
            let artifact_id = id("oak_d_lite");
            let package_name = kind.package_name(&artifact_id);
            assert_eq!(
                ArtifactKind::from_package_name(&package_name),
                Some((kind, artifact_id))
            );
        }
        assert_eq!(ArtifactKind::from_package_name("phoxal-macros"), None);
        assert_eq!(ArtifactKind::from_package_name("service-drive"), None);
        assert_eq!(ArtifactKind::from_package_name("phoxal-service-"), None);
    }

    #[test]
    fn an_artifact_id_is_never_empty() {
        assert!(ArtifactId::new("").is_err());
        assert_eq!(id("drive").to_string(), "drive");
    }

    /// An executable publishes to the `phoxal` registry and nowhere else. The
    /// two rejected cases are the two ways to get this wrong: `publish = false`
    /// hides the package from release-plz's change detection, and anything
    /// naming crates.io (explicitly or by omission) points the executables at
    /// the wrong channel entirely.
    #[test]
    fn an_executable_publishes_only_to_the_phoxal_registry() {
        let manifest = root().join("components/ddsm115/Cargo.toml");
        let check = |publish: Option<&[String]>| {
            OfficialArtifact::validate_publish(
                "phoxal-component-ddsm115",
                publish,
                &root(),
                &manifest,
            )
        };

        check(Some(&["phoxal".to_string()])).expect("publish = [\"phoxal\"] is the one valid form");

        let error = check(Some(&[])).unwrap_err();
        assert!(
            error.to_string().contains("publish = [\"phoxal\"]"),
            "publish = false must be rejected: {error}"
        );

        let error = check(None).unwrap_err();
        assert!(
            error.to_string().contains("defaults to crates.io"),
            "an absent publish field must be rejected: {error}"
        );

        let error = check(Some(&["crates-io".to_string()])).unwrap_err();
        assert!(
            error.to_string().contains("never to crates.io"),
            "crates.io must be rejected: {error}"
        );

        let error = check(Some(&["phoxal".to_string(), "crates-io".to_string()])).unwrap_err();
        assert!(
            error.to_string().contains("publish = [\"phoxal\"]"),
            "a second registry alongside phoxal must be rejected: {error}"
        );
    }

    #[test]
    fn directory_grammar_maps_artifact_kinds() -> Result<()> {
        assert_eq!(
            classify("services/drive/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Service,
                id: id("drive")
            }
        );
        assert_eq!(
            classify("components/ddsm115/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Component,
                id: id("ddsm115")
            }
        );
        // A directory that names no kind is simply not an artifact; the
        // independent simulator tree is outside this workspace's catalogue.
        assert_eq!(
            classify("simulators/mujoco/Cargo.toml")?,
            ManifestClassification::NonArtifact
        );
        Ok(())
    }

    #[test]
    fn nested_crates_under_artifact_roots_are_errors() {
        let err = classify("services/drive/helper/Cargo.toml").unwrap_err();
        assert!(err.to_string().contains("nested under artifact root"));
    }

    /// A component crate lives directly at `components/<id>/Cargo.toml`; a
    /// subdirectory splits one artifact across two paths and is rejected.
    #[test]
    fn nested_crates_under_component_are_errors() {
        let err = classify("components/ddsm115/driver/Cargo.toml").unwrap_err();
        assert!(err.to_string().contains("nested under artifact root"));
    }

    #[test]
    fn library_crate_paths_are_excluded() -> Result<()> {
        assert_eq!(
            classify("phoxal/Cargo.toml")?,
            ManifestClassification::Excluded
        );
        assert_eq!(
            classify("crates/macros/Cargo.toml")?,
            ManifestClassification::Excluded
        );
        assert_eq!(
            classify("crates/fixture/Cargo.toml")?,
            ManifestClassification::Excluded
        );
        Ok(())
    }

    /// `fixture/` holds authored documents and no packages at all, so the
    /// grammar has nothing to say about anything under it. It used to be an
    /// explicit exclusion because a crate lived there too; the exclusion went
    /// away with the crate.
    #[test]
    fn the_authored_fixture_tree_names_no_package() -> Result<()> {
        assert_eq!(
            classify("fixture/components/foo/Cargo.toml")?,
            ManifestClassification::NonArtifact
        );
        Ok(())
    }

    /// `crates/` is reserved for the workspace's own libraries. A package
    /// parked there without being listed would be silently skipped by every
    /// rule in this crate, so the grammar rejects it instead of ignoring it.
    #[test]
    fn an_unlisted_package_under_the_library_root_is_an_error() {
        let err = classify("crates/rogue/Cargo.toml").unwrap_err();
        assert!(
            err.to_string().contains(
                "is not a listed library crate; a library crate is the package \
                          'phoxal-rogue' at 'crates/rogue'"
            ),
            "{err}"
        );

        let err = classify("crates/rogue/inner/Cargo.toml").unwrap_err();
        assert!(
            err.to_string()
                .contains("phoxal-<suffix>' at 'crates/<suffix>"),
            "a package nested deeper than one level names no library crate: {err}"
        );
    }

    #[test]
    fn package_name_must_match_directory_kind_and_id() {
        let err = ArtifactKind::Service
            .validate_package_name(
                "phoxal-component-drive-driver",
                &id("drive"),
                &root(),
                &root().join("services/drive/Cargo.toml"),
            )
            .unwrap_err();
        assert!(err.to_string().contains("expected 'phoxal-service-drive'"));
    }

    #[test]
    fn official_artifacts_require_the_exact_role_metadata() {
        let manifest = root().join("services/drive/Cargo.toml");
        let valid = serde_json::json!({"phoxal": {"kind": "service"}});
        validate_role_metadata(
            ArtifactKind::Service,
            "phoxal-service-drive",
            &valid,
            &root(),
            &manifest,
        )
        .expect("the exact service role should be accepted");

        for metadata in [
            serde_json::json!({}),
            serde_json::json!({"phoxal": {"kind": "component"}}),
            serde_json::json!({"phoxal": {"kind": 1}}),
        ] {
            let error = validate_role_metadata(
                ArtifactKind::Service,
                "phoxal-service-drive",
                &metadata,
                &root(),
                &manifest,
            )
            .unwrap_err();
            assert!(
                error
                    .to_string()
                    .contains("must declare exactly [package.metadata.phoxal] kind = \"service\""),
                "unexpected role diagnostic: {error}"
            );
        }

        validate_role_metadata(
            ArtifactKind::Component,
            "phoxal-component-ddsm115",
            &serde_json::json!({"phoxal": {"kind": "component"}}),
            &root(),
            &root().join("components/ddsm115/Cargo.toml"),
        )
        .expect("the exact component role should be accepted");
        let error = validate_role_metadata(
            ArtifactKind::Component,
            "phoxal-component-ddsm115",
            &serde_json::json!({}),
            &root(),
            &root().join("components/ddsm115/Cargo.toml"),
        )
        .unwrap_err();
        assert!(
            error
                .to_string()
                .contains("must declare exactly [package.metadata.phoxal] kind = \"component\"")
        );
    }

    #[test]
    fn discovery_enforces_library_and_executable_official_artifacts() -> Result<()> {
        let workspace_dir = tempfile::tempdir().context("failed to create temp workspace dir")?;
        let root = workspace_dir.path();

        let specs = super::super::framework_executable::SPECS;
        let mut members = vec!["components/test".to_owned()];
        for spec in specs {
            let manifest = root.join(spec.manifest_path());
            let directory = manifest
                .parent()
                .context("executable manifest has no parent")?;
            members.push(directory.strip_prefix(root)?.display().to_string());
            fs::create_dir_all(directory.join("src"))?;
            fs::write(directory.join("src/main.rs"), "fn main() {}\n")?;
            fs::write(
                manifest,
                format!(
                    "[package]\nname = \"{}\"\nversion = \"0.1.0\"\nedition = \"2024\"\npublish = [\"phoxal\"]\nautobins = false\nautolib = false\n\n[[bin]]\nname = \"{}\"\npath = \"src/main.rs\"\n",
                    spec.package_name(),
                    spec.package_name(),
                ),
            )?;
        }

        fs::write(
            root.join("Cargo.toml"),
            format!(
                "[workspace]\nresolver = \"3\"\nmembers = {}\n",
                serde_json::to_string(&members)?
            ),
        )?;

        let package_dir = root.join("components/test");
        fs::create_dir_all(package_dir.join("src"))?;
        fs::write(
            package_dir.join("src/lib.rs"),
            "//! Deliberately invalid component target fixture.\n",
        )?;
        fs::write(package_dir.join("src/main.rs"), "fn main() {}\n")?;
        fs::write(package_dir.join("src/example.rs"), "fn main() {}\n")?;
        fs::write(package_dir.join("src/test.rs"), "fn main() {}\n")?;
        fs::write(package_dir.join("src/bench.rs"), "fn main() {}\n")?;
        fs::write(package_dir.join("build.rs"), "fn main() {}\n")?;

        let cases = [
            (
                "lib-only",
                "[lib]\npath = \"src/lib.rs\"\n",
                "has 0 binary targets; expected exactly one",
            ),
            (
                "bin-only",
                "[[bin]]\nname = \"phoxal-component-test\"\npath = \"src/main.rs\"\n",
                "has 0 library targets; expected exactly one",
            ),
            (
                "rlib",
                "[lib]\npath = \"src/lib.rs\"\ncrate-type = [\"rlib\"]\n\n[[bin]]\nname = \"phoxal-component-test\"\npath = \"src/main.rs\"\n",
                "target 'phoxal_component_test' has library kind 'rlib'",
            ),
            (
                "proc-macro",
                "[lib]\npath = \"src/lib.rs\"\nproc-macro = true\n\n[[bin]]\nname = \"phoxal-component-test\"\npath = \"src/main.rs\"\n",
                "target 'phoxal_component_test' has library kind 'proc-macro'",
            ),
            (
                "zero-bin",
                "[[example]]\nname = \"validation-example\"\npath = \"src/example.rs\"\n",
                "has 0 library targets; expected exactly one",
            ),
            (
                "wrong-name",
                "[[bin]]\nname = \"wrong-name\"\npath = \"src/main.rs\"\n",
                "its only binary target is 'wrong-name'; expected 'phoxal-component-test'",
            ),
            (
                "multiple-bins",
                "[[bin]]\nname = \"phoxal-component-test\"\npath = \"src/main.rs\"\n\n[[bin]]\nname = \"second\"\npath = \"src/main.rs\"\n",
                "has 2 binary targets; expected exactly one",
            ),
        ];

        for (name, targets, expected_error) in cases {
            fs::write(
                package_dir.join("Cargo.toml"),
                format!(
                    r#"[package]
name = "phoxal-component-test"
version = "0.1.0"
edition = "2024"
license = "AGPL-3.0-only"
publish = ["phoxal"]
description = "Component target validation fixture."
autobins = false
autolib = false

[package.metadata.phoxal]
kind = "component"

{targets}"#
                ),
            )?;

            let error =
                Workspace::discover(MetadataCommand::new().manifest_path(root.join("Cargo.toml")))
                    .unwrap_err();
            assert!(
                error.to_string().contains(expected_error),
                "{name} component fixture produced an unexpected error: {error}"
            );
        }

        fs::write(
            package_dir.join("Cargo.toml"),
            r#"[package]
name = "phoxal-component-test"
version = "0.1.0"
edition = "2024"
license = "AGPL-3.0-only"
publish = ["phoxal"]
description = "Component target validation fixture."
autobins = false
autolib = false
autotests = false
autoexamples = false
autobenches = false
build = "build.rs"

[package.metadata.phoxal]
kind = "component"

[lib]
name = "phoxal_component_test"
path = "src/lib.rs"

[[bin]]
name = "phoxal-component-test"
path = "src/main.rs"

[[test]]
name = "validation-test"
path = "src/test.rs"

[[bench]]
name = "validation-bench"
path = "src/bench.rs"

[[example]]
name = "validation-example"
path = "src/example.rs"
"#,
        )?;
        let workspace =
            Workspace::discover(MetadataCommand::new().manifest_path(root.join("Cargo.toml")))?;
        let [artifact] = workspace.official_artifacts() else {
            bail!("the fixture workspace declares exactly one artifact");
        };
        assert_eq!(artifact.id.to_string(), "test");
        assert_eq!(artifact.package_name(), "phoxal-component-test");
        Ok(())
    }

    #[test]
    fn official_artifact_target_kind_allowlist_is_complete() {
        for kind in [
            TargetKind::Bin,
            TargetKind::Test,
            TargetKind::Bench,
            TargetKind::Example,
            TargetKind::CustomBuild,
        ] {
            assert!(is_allowed_target_kind(&kind), "{kind} must be accepted");
        }
        for kind in [
            TargetKind::Lib,
            TargetKind::RLib,
            TargetKind::DyLib,
            TargetKind::CDyLib,
            TargetKind::StaticLib,
            TargetKind::ProcMacro,
        ] {
            assert!(
                is_library_target_kind(&kind),
                "{kind} must be a library kind"
            );
            assert!(!is_allowed_target_kind(&kind), "{kind} must be rejected");
        }
        let unknown = TargetKind::Unknown("future".to_owned());
        assert!(!is_library_target_kind(&unknown));
        assert!(
            !is_allowed_target_kind(&unknown),
            "unknown target kinds must be rejected"
        );
    }

    #[test]
    fn unknown_target_kind_is_rejected_with_an_unsupported_kind_diagnostic() -> Result<()> {
        let target: Target = serde_json::from_value(serde_json::json!({
            "name": "future-target",
            "kind": ["future"],
            "crate_types": ["bin"],
            "src_path": "/repo/src/main.rs",
            "edition": "2024",
        }))?;

        let error = OfficialArtifact::validate_targets(
            ArtifactKind::Component,
            "phoxal-component-test",
            &[target],
            &root(),
            &root().join("components/test/Cargo.toml"),
        )
        .unwrap_err();
        assert_eq!(
            error.to_string(),
            "phoxal-component-test is an official component package but target 'future-target' has unsupported target kind 'future'; expected bin, test, bench, example, or custom-build"
        );
        Ok(())
    }

    /// The release scope names every artifact by its public identity, so a
    /// listed package that no longer parses back into a kind and id would be a
    /// scope nothing could ever satisfy.
    #[test]
    fn every_package_in_the_release_scope_is_a_well_formed_artifact_identity() {
        for package in OFFICIAL_ARTIFACT_RELEASE_SCOPE {
            let segment = package
                .strip_prefix(PHOXAL_PROVIDER)
                .and_then(|tail| tail.strip_prefix('/'))
                .unwrap_or_else(|| panic!("{package} is not provider-qualified"));
            let (kind, id) =
                ArtifactKind::from_package_name(&format!("{PHOXAL_PACKAGE_PREFIX}{segment}"))
                    .unwrap_or_else(|| panic!("{package} names no artifact kind and id"));
            assert_eq!(kind.package_identity(&id), package);
        }
    }
}
