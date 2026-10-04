//! Simulator-owned installation command/status values shared with developer tooling.
use serde::{Deserialize, Serialize};
use std::path::PathBuf;
/// Application release and location provenance, never executable identity.
#[derive(Clone, Debug, Eq, PartialEq, Serialize, Deserialize)]
pub struct SimulatorArtifactSummary {
    /// Cargo package name.
    pub package: String,
    /// Release provenance.
    pub version: String,
    /// Binary target.
    pub binary: String,
    /// Authored or acquired source provenance.
    pub source: String,
    /// Complete installed application executable location.
    pub executable: PathBuf,
}
/// Machine-readable result of installer install, status, or uninstall.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SimulatorInstallationStatus {
    /// Whether a usable application is selected.
    pub installed: bool,
    /// Managed application root.
    pub root: PathBuf,
    /// Application release provenance.
    pub simulator_version: Option<String>,
    /// Native pairing owned by the simulator distribution.
    pub mujoco_version: Option<String>,
    /// Installed executable location.
    pub executable: Option<PathBuf>,
    /// Complete application provenance.
    pub artifact: Option<SimulatorArtifactSummary>,
}
