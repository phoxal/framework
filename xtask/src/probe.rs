//! Reading a contract surface out of a compiled crate set.
//!
//! A crate states its own surface, so the only way to read one is to compile
//! against the crate and ask it. Both sides of a comparison go through the same
//! mechanism: a small probe project that depends on the framework library and
//! prints the surfaces it states as one JSON array. The baseline side names it
//! at an exact registry version and the workspace side by path, and nothing
//! else about the two differs, so a difference in the output is a difference in
//! the contracts rather than in how they were read.
//!
//! One baseline is the exception, and only until it is behind us: a train that
//! predates the single-crate merge carried the same records in five packages,
//! so that side names all five (see [`crate::legacy`]). Records are unioned
//! either way and identity carries no crate name, so the two sides remain
//! comparable.
//!
//! The probe projects live under `target/`, are their own workspaces, and are
//! rewritten byte-identically on every run, so Cargo rebuilds them only when
//! the crates underneath them actually changed.

use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::index::PublishedTrain;
use crate::legacy;
use crate::surface::{CONTRACT_CRATE, CONTRACT_CRATE_DIRECTORY};

/// Which crate set a surface is read from.
#[derive(Clone, Debug)]
pub(crate) enum Side {
    /// The published train, at an exact registry version.
    Baseline(PublishedTrain),
    /// This workspace, through a path dependency.
    Current,
}

impl Side {
    #[cfg(test)]
    pub(crate) fn baseline(version: semver::Version) -> Self {
        Self::Baseline(PublishedTrain::stated(version, None))
    }

    /// The probe directory this side reuses across runs.
    pub(crate) fn directory_name(&self) -> String {
        match self {
            Self::Baseline(train) => format!("baseline-{}", train.version),
            Self::Current => "current".to_owned(),
        }
    }

    /// The packages this side reads a contract surface out of.
    ///
    /// One, except on a baseline that predates the merge, which is where the
    /// five packages that carried the surface then are named instead.
    fn packages(&self) -> Vec<&'static str> {
        match self {
            Self::Baseline(train) if legacy::precedes_the_single_crate_topology(&train.version) => {
                legacy::CONTRACT_CARRIERS.to_vec()
            }
            Self::Baseline(_) | Self::Current => vec![CONTRACT_CRATE],
        }
    }

    /// How the probe manifest names those packages.
    fn dependencies(&self, workspace_root: &Path) -> String {
        self.packages()
            .into_iter()
            .map(|package| match self {
                Self::Baseline(train) => format!("{package} = \"={}\"\n", train.version),
                Self::Current => format!(
                    "{package} = {{ path = {:?} }}\n",
                    workspace_root.join(CONTRACT_CRATE_DIRECTORY)
                ),
            })
            .collect()
    }
}

impl fmt::Display for Side {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Baseline(train) => write!(formatter, "published {}", train.version),
            Self::Current => formatter.write_str("workspace"),
        }
    }
}

/// What reading one side's surfaces found.
#[derive(Clone, Debug)]
pub(crate) enum Extraction {
    /// Every contract surface that side's crates rendered, in the order the
    /// probe asked for them. The comparison unions the records, so which
    /// document a record arrived in is not carried any further.
    Surfaces(Vec<Value>),
    /// The crates on that side declare no contract surface at all.
    ///
    /// Every train published before the first surface-carrying one is in this
    /// state, which is why it is an outcome rather than a failure. It cannot
    /// recur once a surface-carrying train exists, because a published crate is
    /// never rewritten.
    NoContractSurface,
}

impl Extraction {
    /// Whether a failed probe build is the crate set having no surface to
    /// state, rather than a broken build.
    ///
    /// The probe names `__compat` directly, so a crate set that predates the
    /// module fails to resolve exactly that path and nothing else.
    fn is_missing_contract_surface(build_output: &str) -> bool {
        build_output.contains("could not find `__compat`")
            || build_output.contains("module `__compat` is private")
    }
}

/// Where the checker reads one side's contract surfaces.
///
/// Compiling is the only faithful implementation; a test supplies its own so
/// the comparison rules are proved without one.
pub(crate) trait ContractSurfaces {
    /// Read every contract crate's surface from one crate set.
    fn extract(&self, side: &Side) -> Result<Extraction>;
}

/// Surfaces read by compiling a probe project against one crate set.
pub(crate) struct ProbeSurfaces {
    workspace_root: PathBuf,
}

impl ProbeSurfaces {
    /// Probe the workspace this runner is part of.
    pub(crate) fn for_workspace() -> Result<Self> {
        Ok(Self {
            workspace_root: crate::workspace_root()?,
        })
    }

    /// The probe project for one side, materialized and ready to run.
    fn materialize(&self, side: &Side) -> Result<PathBuf> {
        let directory = self
            .workspace_root
            .join("target")
            .join("xtask-compat")
            .join(side.directory_name());
        fs::create_dir_all(directory.join("src"))
            .with_context(|| format!("failed to create the probe at {}", directory.display()))?;
        write_if_changed(&directory.join("Cargo.toml"), &self.manifest(side))?;
        write_if_changed(&directory.join("src").join("main.rs"), &Self::program(side))?;
        Ok(directory)
    }

    /// The probe manifest: its own workspace, so the enclosing one neither
    /// adopts the generated package nor resolves its dependencies.
    ///
    /// The workspace side takes the crate's default features. A profile decides
    /// which modules are public, never which contracts the crate declares, so
    /// the aggregate the checker reads is the same under any of them and the
    /// cheapest build is the honest one to read it from.
    fn manifest(&self, side: &Side) -> String {
        const HEAD: &str = r#"# Written by `cargo xtask compatibility`. It is regenerated on every
# run, lives under `target/`, and is not tracked.
[workspace]

[package]
name = "phoxal-contract-surface-probe"
version = "0.0.0"
edition = "2024"
publish = false

[dependencies]
"#;
        format!("{HEAD}{}", side.dependencies(&self.workspace_root))
    }

    /// The probe program: one JSON array holding every named crate's own
    /// rendering of its surface, printed to stdout and nothing else.
    fn program(side: &Side) -> String {
        const HEAD: &str = r#"//! Written by `cargo xtask compatibility`. Every crate renders its
//! own surface; this only collects them into one document.

fn main() {
    let surfaces = [
"#;
        const TAIL: &str = r#"    ];
    let mut document = String::from("[");
    for (index, surface) in surfaces.iter().enumerate() {
        if index > 0 {
            document.push(',');
        }
        document.push_str(surface);
    }
    document.push(']');
    println!("{document}");
}
"#;
        let surfaces = side
            .packages()
            .into_iter()
            .map(|package| {
                format!(
                    "        {}::__compat::contract_surface(),\n",
                    package.replace('-', "_")
                )
            })
            .collect::<String>();
        format!("{HEAD}{surfaces}{TAIL}")
    }
}

impl ContractSurfaces for ProbeSurfaces {
    fn extract(&self, side: &Side) -> Result<Extraction> {
        let directory = self.materialize(side)?;
        eprintln!(
            "reading the {side} contract surfaces (compiling {})",
            directory.display()
        );
        let output = Command::new(cargo_command())
            .arg("run")
            .arg("--quiet")
            .current_dir(&directory)
            .output()
            .with_context(|| format!("failed to run the probe at {}", directory.display()))?;
        let diagnostics = String::from_utf8_lossy(&output.stderr).into_owned();
        if !output.status.success() {
            if Extraction::is_missing_contract_surface(&diagnostics) {
                return Ok(Extraction::NoContractSurface);
            }
            bail!(
                "the {side} contract-surface probe failed to build in {}:\n{diagnostics}",
                directory.display()
            );
        }
        let document = String::from_utf8(output.stdout)
            .with_context(|| format!("the {side} probe printed no UTF-8 document"))?;
        serde_json::from_str(document.trim())
            .map(Extraction::Surfaces)
            .with_context(|| format!("the {side} probe printed no surface document: {document}"))
    }
}

/// The Cargo that invoked this runner, so a probe builds with the same
/// toolchain the workspace is being checked with.
pub(crate) fn cargo_command() -> String {
    std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned())
}

/// Write a generated file only when its content changed, so Cargo's own
/// freshness check keeps the probe's build cached between runs.
pub(crate) fn write_if_changed(path: &Path, content: &str) -> Result<()> {
    if fs::read_to_string(path).is_ok_and(|existing| existing == content) {
        return Ok(());
    }
    fs::write(path, content).with_context(|| format!("failed to write {}", path.display()))
}

#[cfg(test)]
mod tests {
    use semver::Version;
    use serde_json::json;

    use super::*;
    use crate::surface::SurfaceSet;

    fn probe() -> ProbeSurfaces {
        ProbeSurfaces {
            workspace_root: PathBuf::from("/workspace"),
        }
    }

    /// The baseline side pins an exact registry version: a caret requirement
    /// would let Cargo resolve a newer train and compare the workspace against
    /// itself.
    #[test]
    fn the_baseline_probe_pins_the_exact_published_version() {
        let manifest = probe().manifest(&Side::baseline(Version::new(0, 66, 1)));
        assert!(manifest.contains("phoxal = \"=0.66.1\""), "{manifest}");
        assert_eq!(
            declared_dependencies(&manifest),
            ["phoxal = \"=0.66.1\""],
            "one library carries the whole surface: {manifest}"
        );
    }

    /// The dependency lines of a rendered probe manifest, in order.
    fn declared_dependencies(manifest: &str) -> Vec<&str> {
        manifest
            .lines()
            .skip_while(|line| *line != "[dependencies]")
            .skip(1)
            .filter(|line| !line.trim().is_empty())
            .collect()
    }

    /// The workspace side reads the crate in front of it, by path.
    #[test]
    fn the_current_probe_names_the_workspace_crate_by_path() {
        let manifest = probe().manifest(&Side::Current);
        assert!(
            manifest.contains("phoxal = { path = \"/workspace/phoxal\" }"),
            "{manifest}"
        );
    }

    /// The one train that predates the merge is read out of the five packages
    /// that actually carried it, at that same version, and the probe asks each
    /// of them for its own surface.
    #[test]
    fn a_baseline_below_the_topology_floor_reads_the_five_retired_packages() {
        let side = Side::baseline(Version::new(0, 65, 0));
        let manifest = probe().manifest(&side);
        assert_eq!(
            declared_dependencies(&manifest),
            crate::legacy::CONTRACT_CARRIERS
                .map(|package| format!("{package} = \"=0.65.0\""))
                .to_vec(),
            "{manifest}"
        );
        let program = ProbeSurfaces::program(&side);
        assert!(
            program.contains("phoxal_runtime_contract::__compat::contract_surface()"),
            "{program}"
        );
        assert_eq!(
            program.matches("contract_surface()").count(),
            5,
            "{program}"
        );
    }

    /// At and above the floor there is one carrier and nothing else is named,
    /// on either side, which is what makes the legacy branch deletable.
    #[test]
    fn a_baseline_at_the_topology_floor_reads_the_one_library() {
        for side in [
            Side::baseline(crate::legacy::topology_floor()),
            Side::Current,
        ] {
            let program = ProbeSurfaces::program(&side);
            assert_eq!(
                program.matches("contract_surface()").count(),
                1,
                "{program}"
            );
            assert!(
                program.contains("phoxal::__compat::contract_surface()"),
                "{program}"
            );
            assert!(!probe().manifest(&side).contains("phoxal-bus"), "{side}");
        }
    }

    /// The merge is invisible to the comparison: the records the retired
    /// packages stated, unioned, are the records the one library states, so a
    /// train that only moved them reports no change.
    #[test]
    fn the_retired_packages_and_the_one_library_compare_as_one_surface() {
        let endpoint = json!({
            "delivery": "state",
            "family": "robot",
            "kind": "state",
            "path": "robot/drive/state",
            "payload": {"fields": [], "kind": "struct"},
            "record": "endpoint",
            "request": Value::Null,
            "response": Value::Null,
        });
        let identifier = json!({
            "name": "encoding",
            "record": "identifier",
            "value": "phoxal/v0;codec=1",
        });
        let retired = vec![
            json!({"records": [endpoint.clone()]}),
            json!({"records": [identifier.clone()]}),
        ];
        let merged = vec![json!({"records": [endpoint, identifier]})];
        let changes = SurfaceSet::read(&retired)
            .expect("the retired surfaces read")
            .changes_to(&SurfaceSet::read(&merged).expect("the merged surface reads"));
        assert!(changes.is_empty(), "{changes:?}");
    }

    /// The probe is its own workspace, so the enclosing one neither adopts the
    /// generated package nor resolves its dependencies.
    #[test]
    fn the_probe_manifest_declares_its_own_workspace() {
        assert!(probe().manifest(&Side::Current).contains("[workspace]"));
    }

    /// Each side reuses one directory, so the second run of a comparison reuses
    /// the first run's build.
    #[test]
    fn each_side_reuses_one_probe_directory() {
        assert_eq!(
            Side::baseline(Version::new(0, 66, 1)).directory_name(),
            "baseline-0.66.1"
        );
        assert_eq!(Side::Current.directory_name(), "current");
    }

    /// The workspace side, read exactly the way a real run reads it: the
    /// library states its surface, and it reads back as records.
    ///
    /// Ignored by default because it compiles the framework library.
    #[test]
    #[ignore = "compiles the workspace framework library"]
    fn the_workspace_states_its_contract_surface() {
        let extracted = ProbeSurfaces::for_workspace()
            .expect("the workspace resolves")
            .extract(&Side::Current)
            .expect("the probe runs");
        let Extraction::Surfaces(surfaces) = extracted else {
            panic!("the workspace crate states its contract surface");
        };
        assert_eq!(surfaces.len(), 1, "{surfaces:?}");
        SurfaceSet::read(&surfaces).expect("the workspace surface reads as records");
    }

    /// A crate set that predates the contract-surface module fails to resolve
    /// exactly that path, which is the one build failure that is an outcome
    /// rather than an error.
    #[test]
    fn a_crate_set_without_a_contract_surface_is_recognized() {
        assert!(Extraction::is_missing_contract_surface(
            "error[E0433]: failed to resolve: could not find `__compat` in `phoxal`"
        ));
        assert!(Extraction::is_missing_contract_surface(
            "error[E0603]: module `__compat` is private"
        ));
        assert!(!Extraction::is_missing_contract_surface(
            "error: linking with `cc` failed: exit status: 1"
        ));
    }
}
