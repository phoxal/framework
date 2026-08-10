//! The official artifact package grammar: where an artifact package lives,
//! what it is called, where it publishes, and which targets it may carry.
//!
//! One artifact is one Cargo package at `{service,component,simulator}/<id>`,
//! producing exactly one binary and no library. Discovery reads the workspace
//! metadata and rejects any package that claims to be an artifact without
//! obeying the grammar, which is what keeps the directory, the crate name and
//! the published identity from drifting apart.

use std::fmt;
use std::path::{Component, Path, PathBuf};

use anyhow::{Context, Result, bail};
use cargo_metadata::semver::Version;
use cargo_metadata::{MetadataCommand, Target, TargetKind};

use crate::{EXCLUDED_TOP_LEVEL_DIRS, LIBRARY_CRATE_DIRS};

/// Official Phoxal packages always use this provider segment in their public
/// `package` identity (`phoxal/<name>`). Third-party packages use their own
/// provider segment; the grammar has no other official provider today.
pub const PHOXAL_PROVIDER: &str = "phoxal";

/// The Cargo `package.name` prefix backing [`PHOXAL_PROVIDER`]: the package
/// `phoxal/service-drive` is the crate `phoxal-service-drive`. The two
/// spellings of the provider are pinned to each other by
/// `the_crate_prefix_spells_the_provider`.
const PHOXAL_PACKAGE_PREFIX: &str = "phoxal-";

/// The kind of an official artifact, named by the top-level directory its
/// packages live under.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub enum ArtifactKind {
    Service,
    /// A component crate: the package-named driver binary plus its assets
    /// (`component.yaml`, `simulation.yaml`, `structure.urdf`, and `meshes/`
    /// if present) in one package. Like every official artifact package, it
    /// has exactly one binary target and no library target. `cargo package`
    /// picks the assets up by default, so they need no inclusion rules.
    Component,
    Simulator,
}

impl ArtifactKind {
    /// Every kind, in the order discovery reports artifacts.
    pub const ALL: [Self; 3] = [Self::Service, Self::Component, Self::Simulator];

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
            Self::Service => "service",
            Self::Component => "component",
            Self::Simulator => "simulator",
        }
    }

    /// The leading segment of both of this kind's names: `service` in
    /// `phoxal/service-drive` and in `phoxal-service-drive`.
    ///
    /// This is published identity. It reaches crates.io, the `phoxal`
    /// registry, and every robot manifest that names an artifact, so it is
    /// frozen independently of where the sources happen to sit in this
    /// repository.
    pub fn name_segment(self) -> &'static str {
        match self {
            Self::Service => "service",
            Self::Component => "component",
            Self::Simulator => "simulator",
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

    /// A package's name is a function of the directory it lives in, so a
    /// mismatch means one of the two moved without the other.
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

    pub fn as_str(&self) -> &str {
        &self.0
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
    pub version: Version,
    pub crate_dir: PathBuf,
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

    /// The release binary's name. Discovery rejects any artifact package whose
    /// one binary target is not named after the package, so the binary and the
    /// package share a name by construction.
    pub fn bin_name(&self) -> String {
        self.package_name()
    }

    /// Every official executable publishes to the `phoxal` registry and only
    /// there.
    ///
    /// This is the Cargo-side guard against an accidental crates.io
    /// publication. It is also what keeps release-plz able to see these
    /// packages at all: a `publish = false` package is invisible to it, and an
    /// invisible package cannot bump the train when it changes.
    fn validate_publish(
        package_name: &str,
        publish: Option<&[String]>,
        root: &Path,
        manifest_path: &Path,
    ) -> Result<()> {
        let declared = publish.unwrap_or_default();
        if declared != [PHOXAL_PROVIDER] {
            bail!(
                "{package_name} is an official executable but {} does not set \
                 publish = [\"{PHOXAL_PROVIDER}\"]; executables publish to the static \
                 {PHOXAL_PROVIDER} registry and never to crates.io, and release-plz must be able \
                 to see them so their changes cut trains. Found: {}",
                relative_display(root, manifest_path),
                if publish.is_none() {
                    "no publish field (defaults to crates.io)".to_string()
                } else {
                    format!("publish = {declared:?}")
                }
            );
        }
        Ok(())
    }

    /// Enforces the executable-only target convention for every official
    /// artifact package: exactly one package-named binary and no library
    /// target. Components ship their authored assets beside that binary in the
    /// same archive; they do not need a Cargo library target for discovery or
    /// packaging.
    fn validate_targets(package_name: &str, targets: &[Target]) -> Result<()> {
        if let Some((target, target_kind)) = targets
            .iter()
            .find_map(|target| unsupported_target_kind(target).map(|kind| (target, kind)))
        {
            if is_library_target_kind(target_kind) {
                bail!(
                    "{package_name} is an official artifact package but target '{}' has library kind '{target_kind}'",
                    target.name
                );
            }
            bail!(
                "{package_name} is an official artifact package but target '{}' has unsupported target kind '{target_kind}'; expected bin, test, bench, example, or custom-build",
                target.name
            );
        }

        let binary_targets: Vec<_> = targets
            .iter()
            .filter(|target| target.is_kind(TargetKind::Bin))
            .collect();

        let [binary_target] = binary_targets.as_slice() else {
            bail!(
                "{package_name} is an official artifact package but has {} binary targets; expected exactly one",
                binary_targets.len()
            );
        };
        if binary_target.name != package_name {
            bail!(
                "{package_name} is an official artifact package but its only binary target is '{}'; expected '{package_name}'",
                binary_target.name
            );
        }

        Ok(())
    }
}

/// The official artifacts a workspace declares, as discovery found them.
#[derive(Debug)]
pub struct Workspace {
    official_artifacts: Vec<OfficialArtifact>,
}

impl Workspace {
    /// Reads `command`'s workspace metadata and validates every package in it
    /// against the grammar, failing on the first violation.
    pub fn discover(command: &mut MetadataCommand) -> Result<Self> {
        let metadata = command
            .no_deps()
            .exec()
            .context("failed to read cargo metadata")?;
        let root = metadata.workspace_root.clone().into_std_path_buf();
        let mut official_artifacts = Vec::new();

        for package in metadata.workspace_packages() {
            let package_name = package.name.to_string();
            let manifest_path = package.manifest_path.clone().into_std_path_buf();
            let manifest = ManifestClassification::classify(&root, &manifest_path)
                .with_context(|| format!("failed to classify {}", manifest_path.display()))?;
            let ManifestClassification::Artifact { kind, id } = manifest else {
                if let Some((prefix_kind, prefix_id)) =
                    ArtifactKind::from_package_name(&package_name)
                {
                    bail!(
                        "{package_name} uses the {prefix_kind} artifact package prefix with id \
                         '{prefix_id}' but its manifest path {} is outside the exact \
                         directory grammar for that kind",
                        relative_display(&root, &manifest_path)
                    );
                }
                continue;
            };

            kind.validate_package_name(&package_name, &id, &root, &manifest_path)?;
            OfficialArtifact::validate_publish(
                &package_name,
                package.publish.as_deref(),
                &root,
                &manifest_path,
            )?;
            OfficialArtifact::validate_targets(&package_name, &package.targets)?;
            let crate_dir = manifest_path
                .parent()
                .with_context(|| format!("{package_name} manifest has no parent directory"))?
                .to_path_buf();

            official_artifacts.push(OfficialArtifact {
                kind,
                id,
                version: package.version.clone(),
                crate_dir,
            });
        }

        official_artifacts.sort_by(|left, right| {
            left.kind
                .cmp(&right.kind)
                .then_with(|| left.id.cmp(&right.id))
        });

        Ok(Self { official_artifacts })
    }

    pub fn official_artifacts(&self) -> &[OfficialArtifact] {
        &self.official_artifacts
    }
}

/// What a workspace member's manifest path says the package is.
#[derive(Debug, Eq, PartialEq)]
enum ManifestClassification {
    /// An official artifact package, at the exact path its kind and id require.
    Artifact { kind: ArtifactKind, id: ArtifactId },
    /// A path the grammar deliberately says nothing about: a library crate at
    /// its own root, this policy crate, or a manifest fixture.
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

        if EXCLUDED_TOP_LEVEL_DIRS.contains(&top_level) {
            return Ok(Self::Excluded);
        }
        if LIBRARY_CRATE_DIRS.contains(&top_level)
            && components.as_slice() == [top_level, "Cargo.toml"]
        {
            return Ok(Self::Excluded);
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

fn unsupported_target_kind(target: &Target) -> Option<&TargetKind> {
    target
        .kind
        .iter()
        .find(|kind| !is_allowed_artifact_target_kind(kind))
}

fn is_allowed_artifact_target_kind(kind: &TargetKind) -> bool {
    matches!(
        kind,
        TargetKind::Bin
            | TargetKind::Test
            | TargetKind::Bench
            | TargetKind::Example
            | TargetKind::CustomBuild
    )
}

/// Whether a target kind produces a library. Official artifact packages carry
/// none; the workspace's library crates are exactly the packages that do.
pub(crate) fn is_library_target_kind(kind: &TargetKind) -> bool {
    matches!(
        kind,
        TargetKind::Lib
            | TargetKind::RLib
            | TargetKind::DyLib
            | TargetKind::CDyLib
            | TargetKind::StaticLib
            | TargetKind::ProcMacro
    )
}

fn relative_display(root: &Path, path: &Path) -> String {
    path.strip_prefix(root)
        .unwrap_or(path)
        .display()
        .to_string()
}

#[cfg(test)]
mod tests {
    use std::fs;

    use crate::workspace_root;

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
            [
                ("service", "service"),
                ("component", "component"),
                ("simulator", "simulator"),
            ]
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
            "'library' names no artifact kind; expected one of service, component, simulator"
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
        assert_eq!(
            ArtifactKind::Simulator.package_identity(&id("webots-controller")),
            "phoxal/simulator-webots-controller"
        );
    }

    /// The public identity and the crate name are two renderings of one
    /// identity, and a package name parses back into the pair that rendered it.
    #[test]
    fn a_package_name_parses_back_into_its_kind_and_id() {
        for kind in ArtifactKind::ALL {
            let artifact_id = id("webots-controller");
            let package_name = kind.package_name(&artifact_id);
            assert_eq!(
                ArtifactKind::from_package_name(&package_name),
                Some((kind, artifact_id))
            );
        }
        assert_eq!(ArtifactKind::from_package_name("phoxal-bus"), None);
        assert_eq!(ArtifactKind::from_package_name("service-drive"), None);
        assert_eq!(ArtifactKind::from_package_name("phoxal-service-"), None);
    }

    #[test]
    fn an_artifact_id_is_never_empty() {
        assert!(ArtifactId::new("").is_err());
        assert_eq!(id("drive").as_str(), "drive");
    }

    /// An executable publishes to the `phoxal` registry and nowhere else. The
    /// two rejected cases are the two ways to get this wrong: `publish = false`
    /// hides the package from release-plz so its changes stop cutting trains,
    /// and anything naming crates.io (explicitly or by omission) points the
    /// executables at the wrong channel entirely.
    #[test]
    fn an_executable_publishes_only_to_the_phoxal_registry() {
        let manifest = root().join("simulator/webots-controller/Cargo.toml");
        let check = |publish: Option<&[String]>| {
            OfficialArtifact::validate_publish(
                "phoxal-simulator-webots-controller",
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
            classify("service/drive/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Service,
                id: id("drive")
            }
        );
        assert_eq!(
            classify("component/ddsm115/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Component,
                id: id("ddsm115")
            }
        );
        assert_eq!(
            classify("simulator/webots/Cargo.toml")?,
            ManifestClassification::Artifact {
                kind: ArtifactKind::Simulator,
                id: id("webots")
            }
        );
        Ok(())
    }

    #[test]
    fn nested_crates_under_artifact_roots_are_errors() {
        let err = classify("service/drive/helper/Cargo.toml").unwrap_err();
        assert!(err.to_string().contains("nested under artifact root"));
    }

    /// A component crate lives directly at `component/<id>/Cargo.toml`; a
    /// subdirectory splits one artifact across two paths and is rejected.
    #[test]
    fn nested_crates_under_component_are_errors() {
        let err = classify("component/ddsm115/driver/Cargo.toml").unwrap_err();
        assert!(err.to_string().contains("nested under artifact root"));
    }

    #[test]
    fn library_policy_and_fixture_paths_are_excluded() -> Result<()> {
        assert_eq!(
            classify("phoxal/Cargo.toml")?,
            ManifestClassification::Excluded
        );
        assert_eq!(
            classify("workspace-policy/Cargo.toml")?,
            ManifestClassification::Excluded
        );
        assert_eq!(
            classify("fixture/component/foo/Cargo.toml")?,
            ManifestClassification::Excluded
        );
        Ok(())
    }

    #[test]
    fn package_name_must_match_directory_kind_and_id() {
        let err = ArtifactKind::Service
            .validate_package_name(
                "phoxal-component-drive-driver",
                &id("drive"),
                &root(),
                &root().join("service/drive/Cargo.toml"),
            )
            .unwrap_err();
        assert!(err.to_string().contains("expected 'phoxal-service-drive'"));
    }

    #[test]
    fn discovery_enforces_executable_only_official_artifacts() -> Result<()> {
        let workspace_dir = tempfile::tempdir().context("failed to create temp workspace dir")?;
        let root = workspace_dir.path();

        fs::write(
            root.join("Cargo.toml"),
            r#"[workspace]
resolver = "3"
members = ["component/test"]
"#,
        )?;

        let package_dir = root.join("component/test");
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
                "target 'phoxal_component_test' has library kind 'lib'",
            ),
            (
                "mixed",
                "[lib]\npath = \"src/lib.rs\"\n\n[[bin]]\nname = \"phoxal-component-test\"\npath = \"src/main.rs\"\n",
                "target 'phoxal_component_test' has library kind 'lib'",
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
                "has 0 binary targets; expected exactly one",
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
        assert_eq!(artifact.id.as_str(), "test");
        assert_eq!(artifact.package_name(), "phoxal-component-test");
        assert_eq!(artifact.bin_name(), "phoxal-component-test");
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
            assert!(
                is_allowed_artifact_target_kind(&kind),
                "{kind} must be accepted"
            );
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
            assert!(
                !is_allowed_artifact_target_kind(&kind),
                "{kind} must be rejected"
            );
        }
        let unknown = TargetKind::Unknown("future".to_owned());
        assert!(!is_library_target_kind(&unknown));
        assert!(
            !is_allowed_artifact_target_kind(&unknown),
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

        let error =
            OfficialArtifact::validate_targets("phoxal-component-test", &[target]).unwrap_err();
        assert_eq!(
            error.to_string(),
            "phoxal-component-test is an official artifact package but target 'future-target' has unsupported target kind 'future'; expected bin, test, bench, example, or custom-build"
        );
        Ok(())
    }

    #[test]
    fn real_workspace_release_scope_is_valid() -> Result<()> {
        use std::collections::BTreeSet;

        let workspace_manifest = workspace_root()?.join("Cargo.toml");
        let workspace =
            Workspace::discover(MetadataCommand::new().manifest_path(workspace_manifest))?;

        let actual = workspace
            .official_artifacts()
            .iter()
            .map(OfficialArtifact::package)
            .collect::<BTreeSet<_>>();
        let expected = BTreeSet::from([
            "phoxal/component-bno085",
            "phoxal/component-ddsm115",
            "phoxal/component-oak_d_lite",
            "phoxal/component-vl53l1x",
            "phoxal/component-zed_f9p",
            "phoxal/service-drive",
            "phoxal/service-frame",
            "phoxal/service-joint",
            "phoxal/service-localize",
            "phoxal/service-map",
            "phoxal/service-motion",
            "phoxal/service-navigation",
            "phoxal/service-odometry",
            "phoxal/service-perception",
            "phoxal/service-safety",
            "phoxal/service-video",
            "phoxal/simulator-webots-controller",
        ])
        .into_iter()
        .map(str::to_owned)
        .collect::<BTreeSet<_>>();
        assert_eq!(actual, expected);
        assert!(
            !actual.contains("phoxal/service-power"),
            "the removed power service must not remain in the release scope"
        );

        assert_eq!(
            workspace
                .official_artifacts()
                .iter()
                .filter(|artifact| artifact.kind == ArtifactKind::Service)
                .count(),
            11
        );
        let simulators = workspace
            .official_artifacts()
            .iter()
            .filter(|artifact| artifact.kind == ArtifactKind::Simulator)
            .map(OfficialArtifact::package)
            .collect::<Vec<_>>();
        assert_eq!(simulators, ["phoxal/simulator-webots-controller"]);
        Ok(())
    }
}
