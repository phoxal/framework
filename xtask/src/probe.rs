//! Reading a contract surface out of a compiled crate set.
//!
//! A crate states its own surface, so the only way to read one is to compile
//! against the crate and ask it. Both sides of a comparison go through the same
//! mechanism: a small probe project that depends on the five contract crates
//! and prints their surfaces as one JSON document. The baseline side names them
//! at exact registry versions and the workspace side by path, and nothing else
//! about the two differs, so a difference in the output is a difference in the
//! contracts rather than in how they were read.
//!
//! The probe projects live under `target/`, are their own workspaces, and are
//! rewritten byte-identically on every run, so Cargo rebuilds them only when
//! the crates underneath them actually changed.

use std::collections::BTreeMap;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use anyhow::{Context, Result, bail};
use serde_json::Value;

use crate::index::PublishedTrain;
use crate::surface::CONTRACT_CRATES;

/// Which crate set a surface is read from.
#[derive(Clone, Debug)]
pub(crate) enum Side {
    /// The published train, at exact registry versions and using the actual
    /// package selected for each stable carrier.
    Baseline(PublishedTrain),
    /// This workspace, through path dependencies.
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

    /// How the probe manifest names the contract crates.
    fn dependencies(&self, workspace_root: &Path) -> Result<String> {
        CONTRACT_CRATES
            .iter()
            .map(|contract_crate| match self {
                Self::Baseline(train) => {
                    let package = train.selected_package(contract_crate.carrier)?;
                    if package == contract_crate.carrier {
                        Ok(format!(
                            "{} = \"={}\"\n",
                            contract_crate.carrier, train.version
                        ))
                    } else {
                        Ok(format!(
                            "{} = {{ package = {:?}, version = \"={}\" }}\n",
                            contract_crate.carrier, package, train.version
                        ))
                    }
                }
                Self::Current => {
                    let path = workspace_root.join(contract_crate.workspace_directory);
                    if contract_crate.workspace_package == contract_crate.carrier {
                        Ok(format!(
                            "{} = {{ path = {:?} }}\n",
                            contract_crate.carrier, path
                        ))
                    } else {
                        Ok(format!(
                            "{} = {{ package = {:?}, path = {:?} }}\n",
                            contract_crate.carrier, contract_crate.workspace_package, path
                        ))
                    }
                }
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
    /// The rendered surface of every contract crate, by crate name.
    Surfaces(BTreeMap<String, Value>),
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
        write_if_changed(&directory.join("Cargo.toml"), &self.manifest(side)?)?;
        write_if_changed(&directory.join("src").join("main.rs"), &Self::program())?;
        Ok(directory)
    }

    /// The probe manifest: its own workspace, so the enclosing one neither
    /// adopts the generated package nor resolves its dependencies.
    fn manifest(&self, side: &Side) -> Result<String> {
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
        Ok(format!(
            "{HEAD}{}",
            side.dependencies(&self.workspace_root)?
        ))
    }

    /// The probe program: one JSON object holding every crate's own rendering
    /// of its surface, printed to stdout and nothing else.
    fn program() -> String {
        const HEAD: &str = r#"//! Written by `cargo xtask compatibility`. Every crate renders its
//! own surface; this only collects them into one document.

fn main() {
    let surfaces = [
"#;
        const TAIL: &str = r#"    ];
    let mut document = String::from("{");
    for (index, (name, surface)) in surfaces.iter().enumerate() {
        if index > 0 {
            document.push(',');
        }
        document.push('"');
        document.push_str(name);
        document.push_str("\":");
        document.push_str(surface);
    }
    document.push('}');
    println!("{document}");
}
"#;
        let surfaces = CONTRACT_CRATES
            .iter()
            .map(|contract_crate| {
                format!(
                    "        (\"{}\", {}::__compat::contract_surface()),\n",
                    contract_crate.carrier,
                    contract_crate.module()
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

    use super::*;
    use crate::surface::SurfaceSet;

    fn probe() -> ProbeSurfaces {
        ProbeSurfaces {
            workspace_root: PathBuf::from("/workspace"),
        }
    }

    /// The baseline side pins exact registry versions: a caret requirement
    /// would let Cargo resolve a newer train and compare the workspace against
    /// itself.
    #[test]
    fn the_baseline_probe_pins_exact_published_versions() {
        let manifest = probe()
            .manifest(&Side::baseline(Version::new(0, 58, 1)))
            .expect("the manifest renders");
        assert!(manifest.contains("phoxal = \"=0.58.1\""), "{manifest}");
        assert!(
            manifest.contains("phoxal-runtime-contract = \"=0.58.1\""),
            "{manifest}"
        );
    }

    /// The workspace side reads the crates in front of it, by path.
    #[test]
    fn the_current_probe_names_the_workspace_crates_by_path() {
        let manifest = probe()
            .manifest(&Side::Current)
            .expect("the manifest renders");
        assert!(
            manifest.contains(
                "phoxal-protocol = { package = \"phoxal-api\", path = \"/workspace/crates/api\" }"
            ),
            "{manifest}"
        );
    }

    /// The first renamed baseline aliases the predecessor package privately to
    /// the stable carrier, so both sides render records under one identity.
    #[test]
    fn the_first_baseline_privately_aliases_phoxal_api_as_phoxal_protocol() {
        let manifest = probe()
            .manifest(&Side::baseline(Version::new(0, 58, 1)))
            .expect("the manifest renders");
        assert!(
            manifest
                .contains("phoxal-protocol = { package = \"phoxal-api\", version = \"=0.58.1\" }"),
            "{manifest}"
        );
        assert!(!manifest.contains("\nphoxal-api ="), "{manifest}");
    }

    /// The probe is its own workspace, so the enclosing one neither adopts the
    /// generated package nor resolves its dependencies.
    #[test]
    fn the_probe_manifest_declares_its_own_workspace() {
        assert!(
            probe()
                .manifest(&Side::Current)
                .expect("the manifest renders")
                .contains("[workspace]")
        );
    }

    /// Every contract crate is asked for its own surface, under the name the
    /// comparison indexes it by.
    #[test]
    fn the_probe_program_collects_every_contract_crate() {
        let program = ProbeSurfaces::program();
        for contract_crate in CONTRACT_CRATES {
            assert!(
                program.contains(&format!(
                    "(\"{}\", {}::__compat::contract_surface())",
                    contract_crate.carrier,
                    contract_crate.module()
                )),
                "{program}"
            );
        }
    }

    /// Each side reuses one directory, so the second run of a comparison reuses
    /// the first run's build.
    #[test]
    fn each_side_reuses_one_probe_directory() {
        assert_eq!(
            Side::baseline(Version::new(0, 58, 1)).directory_name(),
            "baseline-0.58.1"
        );
        assert_eq!(Side::Current.directory_name(), "current");
    }

    /// The workspace side, read exactly the way a real run reads it: every
    /// contract crate states a surface, and the set reads back as records.
    ///
    /// Ignored by default because it compiles the whole contract crate set.
    #[test]
    #[ignore = "compiles the workspace contract crates"]
    fn the_workspace_states_every_contract_surface() {
        let extracted = ProbeSurfaces::for_workspace()
            .expect("the workspace resolves")
            .extract(&Side::Current)
            .expect("the probe runs");
        let Extraction::Surfaces(surfaces) = extracted else {
            panic!("the workspace crates state their contract surfaces");
        };
        for contract_crate in CONTRACT_CRATES {
            assert!(
                surfaces.contains_key(contract_crate.carrier),
                "{} stated no surface",
                contract_crate.carrier
            );
        }
        SurfaceSet::read(&surfaces).expect("the workspace surfaces read as records");
    }

    /// A crate set that predates the contract-surface module fails to resolve
    /// exactly that path, which is the one build failure that is an outcome
    /// rather than an error.
    #[test]
    fn a_crate_set_without_a_contract_surface_is_recognized() {
        assert!(Extraction::is_missing_contract_surface(
            "error[E0433]: failed to resolve: could not find `__compat` in `phoxal_api`"
        ));
        assert!(Extraction::is_missing_contract_surface(
            "error[E0603]: module `__compat` is private"
        ));
        assert!(!Extraction::is_missing_contract_surface(
            "error: linking with `cc` failed: exit status: 1"
        ));
    }
}
