//! The Rust toolchain floor a train requires, and what raising it costs.
//!
//! A published crate states the oldest toolchain that can build it. Raising
//! that floor breaks every downstream build still on the previous toolchain,
//! which is a break of the same kind as a removed endpoint: the consumer did
//! nothing and stopped working. So the floor is a compatibility axis, read from
//! the same registry entry the baseline train is resolved from and gated with
//! the same release arithmetic.

use std::cmp::Ordering;
use std::fmt;
use std::fs;
use std::path::Path;

use anyhow::{Context, Result, bail};

use crate::release::ReleaseStep;

/// A `rust-version` floor, as a manifest or a registry index states it.
///
/// Cargo accepts one to three numeric segments, so `1.88` and `1.88.0` are one
/// floor spelled two ways. Comparison is on the numbers; the spelling is kept
/// only so a diagnostic quotes what was actually written.
#[derive(Clone, Debug)]
pub(crate) struct RustVersion {
    spelling: String,
    ordinal: (u64, u64, u64),
}

impl RustVersion {
    /// Read one floor, or say why the value is not one.
    pub(crate) fn parse(value: &str) -> Result<Self> {
        let spelling = value.trim();
        let segments = spelling.split('.').collect::<Vec<_>>();
        if segments.is_empty() || segments.len() > 3 {
            bail!(
                "`{spelling}` is not a rust-version: a floor is one to three numeric segments, \
                 such as 1.88 or 1.88.0"
            );
        }
        let mut ordinal = [0_u64; 3];
        for (index, segment) in segments.iter().enumerate() {
            if segment.is_empty() || !segment.bytes().all(|byte| byte.is_ascii_digit()) {
                bail!(
                    "`{spelling}` is not a rust-version: `{segment}` is not a decimal segment. A \
                     floor carries no channel, pre-release, or suffix."
                );
            }
            ordinal[index] = segment
                .parse::<u64>()
                .with_context(|| format!("`{spelling}` states a segment no toolchain can have"))?;
        }
        Ok(Self {
            spelling: spelling.to_owned(),
            ordinal: (ordinal[0], ordinal[1], ordinal[2]),
        })
    }
}

impl PartialEq for RustVersion {
    /// Two floors are the same floor when they demand the same toolchain, not
    /// when they are spelled the same way.
    fn eq(&self, other: &Self) -> bool {
        self.ordinal == other.ordinal
    }
}

impl Eq for RustVersion {}

impl PartialOrd for RustVersion {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for RustVersion {
    fn cmp(&self, other: &Self) -> Ordering {
        self.ordinal.cmp(&other.ordinal)
    }
}

impl fmt::Display for RustVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.spelling)
    }
}

/// The floor this workspace declares, read from `[workspace.package]`.
///
/// Every member inherits `rust-version.workspace = true`, so the root manifest
/// is where the workspace states its floor and the only place to read it. It is
/// parsed by hand rather than through a TOML dependency: one key under one
/// table is a smaller thing to read than a parser is to carry, and the checker
/// stays as dependency-light as the probes it builds.
pub(crate) fn workspace_floor(workspace_root: &Path) -> Result<RustVersion> {
    let path = workspace_root.join("Cargo.toml");
    let manifest =
        fs::read_to_string(&path).with_context(|| format!("failed to read {}", path.display()))?;
    let stated = manifest
        .lines()
        .skip_while(|line| line.trim() != "[workspace.package]")
        .skip(1)
        .take_while(|line| !line.trim_start().starts_with('['))
        .find_map(|line| line.trim().strip_prefix("rust-version = "))
        .map(|value| value.trim().trim_matches('"'))
        .with_context(|| {
            format!(
                "{} declares no `rust-version` under [workspace.package], so this workspace \
                 states no toolchain floor to compare",
                path.display()
            )
        })?;
    RustVersion::parse(stated)
        .with_context(|| format!("{} declares an unreadable rust-version", path.display()))
}

/// What this workspace does to the toolchain floor the published train stated.
#[derive(Clone, Debug)]
pub(crate) enum ToolchainFloor {
    /// The published train states no floor, so there is none to compare.
    ///
    /// Only a crate published before the registry index carried the field is in
    /// this state; it cannot recur, because a published crate is never
    /// rewritten.
    Unstated { workspace: RustVersion },
    /// The workspace demands no newer toolchain than the published train did.
    Held {
        baseline: RustVersion,
        workspace: RustVersion,
    },
    /// The workspace demands a newer toolchain than the published train did.
    Raised {
        baseline: RustVersion,
        workspace: RustVersion,
    },
}

impl ToolchainFloor {
    /// Classify the workspace floor against the published train's.
    pub(crate) fn between(baseline: Option<RustVersion>, workspace: RustVersion) -> Self {
        match baseline {
            None => Self::Unstated { workspace },
            Some(baseline) if workspace > baseline => Self::Raised {
                baseline,
                workspace,
            },
            Some(baseline) => Self::Held {
                baseline,
                workspace,
            },
        }
    }

    /// The smallest release a raised floor may go out in.
    ///
    /// A raised floor takes a minor in both SemVer eras. Pre-1.0 the minor is
    /// the compatibility line, so a break of any kind lands there. From 1.0 on
    /// it is a minor rather than a major by deliberate convention: the API a
    /// peer speaks is untouched, and what changed is the toolchain a consumer
    /// needs to build the same API. A lowered floor asks nothing of anybody, so
    /// it constrains no release.
    pub(crate) fn required_step(&self) -> Option<ReleaseStep> {
        match self {
            Self::Raised { .. } => Some(ReleaseStep::Minor),
            Self::Unstated { .. } | Self::Held { .. } => None,
        }
    }
}

impl fmt::Display for ToolchainFloor {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Unstated { workspace } => write!(
                formatter,
                "{workspace} (the published train states no rust-version, so no floor is compared)"
            ),
            Self::Held {
                baseline,
                workspace,
            } if workspace == baseline => {
                write!(
                    formatter,
                    "{workspace} (unchanged from the published train)"
                )
            }
            Self::Held {
                baseline,
                workspace,
            } => write!(
                formatter,
                "{workspace} (lowered from the published {baseline}; no release impact)"
            ),
            Self::Raised {
                baseline,
                workspace,
            } => write!(
                formatter,
                "{workspace} (raised from the published {baseline}; requires at least a minor \
                 release)"
            ),
        }
    }
}

#[cfg(test)]
mod tests {
    use std::fs;

    use super::*;

    fn floor(value: &str) -> RustVersion {
        RustVersion::parse(value).expect("the floor parses")
    }

    /// A floor is what it demands, not how it is written: the two-segment and
    /// three-segment spellings of one toolchain are one floor.
    #[test]
    fn a_floor_compares_by_toolchain_and_prints_its_own_spelling() {
        assert_eq!(floor("1.88"), floor("1.88.0"));
        assert!(floor("1.90") > floor("1.88"));
        assert!(floor("1.88.1") > floor("1.88"));
        assert!(floor("2.0") > floor("1.99.99"));
        assert_eq!(floor("1.88").to_string(), "1.88");
        assert_eq!(floor(" 1.88.0 ").to_string(), "1.88.0");
    }

    /// A value that is not a plain numeric floor is refused rather than
    /// silently ordered against real ones.
    #[test]
    fn a_value_that_is_not_a_floor_is_refused() {
        for value in [
            "",
            "1.88.0.1",
            "1.x",
            "nightly",
            "1.88-beta",
            "v1.88",
            "1..0",
        ] {
            assert!(
                RustVersion::parse(value).is_err(),
                "{value} must not read as a rust-version"
            );
        }
    }

    /// A raised floor is a minor in both eras; nothing else constrains a
    /// release.
    #[test]
    fn only_a_raised_floor_requires_a_release() {
        assert_eq!(
            ToolchainFloor::between(Some(floor("1.88")), floor("1.90")).required_step(),
            Some(ReleaseStep::Minor)
        );
        assert_eq!(
            ToolchainFloor::between(Some(floor("1.88")), floor("1.88.0")).required_step(),
            None
        );
        assert_eq!(
            ToolchainFloor::between(Some(floor("1.90")), floor("1.88")).required_step(),
            None
        );
        assert_eq!(
            ToolchainFloor::between(None, floor("1.88")).required_step(),
            None
        );
    }

    /// Each outcome says which way the floor moved, so a report states the fact
    /// rather than only its verdict.
    #[test]
    fn every_floor_outcome_names_what_it_found() {
        let rendered = |baseline: Option<&str>, workspace: &str| -> String {
            ToolchainFloor::between(baseline.map(floor), floor(workspace)).to_string()
        };
        assert!(rendered(Some("1.88"), "1.90").contains("raised from the published 1.88"));
        assert!(rendered(Some("1.88"), "1.90").contains("at least a minor"));
        assert!(rendered(Some("1.88"), "1.88").contains("unchanged"));
        assert!(rendered(Some("1.90"), "1.88").contains("lowered from the published 1.90"));
        assert!(rendered(None, "1.88").contains("states no rust-version"));
    }

    /// The floor is read from the one table that declares it, and a manifest
    /// that declares none says so instead of defaulting to something.
    #[test]
    fn the_workspace_floor_is_read_from_the_root_manifest() {
        let root = tempfile::tempdir().expect("a temporary workspace");
        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.1.0\"\nrust-version = \"1.90\"\n\n\
             [workspace]\nmembers = []\n",
        )
        .expect("the manifest writes");
        assert_eq!(
            workspace_floor(root.path()).expect("the floor reads"),
            floor("1.90")
        );

        fs::write(
            root.path().join("Cargo.toml"),
            "[workspace.package]\nversion = \"0.1.0\"\n\n[workspace]\nrust-version = \"1.90\"\n",
        )
        .expect("the manifest writes");
        let failure = workspace_floor(root.path())
            .expect_err("a workspace with no floor cannot state one")
            .to_string();
        assert!(failure.contains("no `rust-version`"), "{failure}");
    }

    /// This workspace's own floor reads, so the checker cannot ship unable to
    /// find the value it gates on.
    #[test]
    fn this_workspace_states_a_floor() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .expect("the runner sits in the workspace");
        workspace_floor(root).expect("this workspace declares a rust-version");
    }
}
