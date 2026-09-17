//! Wire types for the case-host control channel.
//!
//! Plan §9 specifies a small, versioned, bounded private protocol
//! between the tool (case host) and the generated scenario harness.
//! Both sides speak only the types declared here; nothing else is
//! exchanged. The harness never sends a Program to the tool without
//! first reading a Probe, and the tool never sends Evidence without
//! first reading the harness's Program.
//!
//! Every Program envelope rides the existing canonical wire format
//! (the bytes [`crate::scenario::program::Program::program_bytes`]
//! produces) embedded as a base64 string. This avoids JSON
//! double-encoding while keeping the framed payload a single JSON
//! document. Both sides reconstruct with
//! [`crate::scenario::program::Program::decode`].

use std::path::PathBuf;

use base64::Engine as _;
use serde::{Deserialize, Serialize};

use crate::scenario::participant::StepOutcome;
use crate::scenario::program::Program;
use crate::scenario::results::CaptureRecord;

/// The current protocol version. Both sides reject any other value on
/// the first `Hello` frame.
pub const PROTOCOL_VERSION: u32 = 1;

const BASE64_ENGINE: base64::engine::general_purpose::GeneralPurpose =
    base64::engine::general_purpose::STANDARD;

/// One message the tool sends to the harness. The harness reads
/// these in order: `Hello`, then `Probe`, then `Evidence`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HarnessRequest {
    /// First frame the tool sends. The harness replies with
    /// [`HarnessResponse::HelloAck`] after validating the version.
    Hello {
        /// Always [`PROTOCOL_VERSION`]; mismatches fail closed.
        version: u32,
    },
    /// Tool sends after the harness has reported its planned scene
    /// and authored duration. The harness validates the plan against
    /// the supplied quantum and replies with [`HarnessResponse::Program`].
    Probe {
        /// Probed quantum in nanoseconds. Zero is rejected by the
        /// harness as a protocol violation.
        quantum_ns: u64,
        /// Canonical simulator model identity. Empty is rejected.
        model_identity: String,
    },
    /// Tool sends after the supervised run finished. The harness
    /// seals the run and replies with [`HarnessResponse::Verdict`].
    Evidence {
        /// Lifecycle-observed evidence the tool collected from the
        /// supervisor/simulator lifecycle.
        report: LifecycleReport,
        /// Whether the lifecycle cleanup completed without error.
        cleanup_succeeded: bool,
        /// Whether the supervisor admitted the bundle, started the
        /// simulator, and exited cleanly. False here forces a
        /// failing verdict.
        lifecycle_passing: bool,
    },
}

/// One message the harness sends to the tool. The tool reads these
/// in order: `HelloAck`, then `Open`, then `Program`, then `Verdict`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum HarnessResponse {
    /// Reply to the tool's first frame. The harness also reports
    /// the scenario summary so the tool can log it.
    HelloAck {
        /// Always [`PROTOCOL_VERSION`].
        version: u32,
        scenario: ScenarioSummary,
    },
    /// The harness reports the planned scenario it retained. The
    /// tool uses this to drive its probe.
    Open {
        scenario: ScenarioSummary,
    },
    /// The harness sends the wire-stable program after validating
    /// the plan against the tool's probed quantum.
    Program {
        /// Canonical program bytes (the existing
        /// [`crate::scenario::program::Program::program_bytes`]
        /// envelope), base64-encoded. The tool reconstructs with
        /// [`crate::scenario::program::Program::decode`].
        program_envelope_b64: String,
        /// Computed transition count the tool uses to size the run.
        transitions: u32,
        /// Echoes the model identity the tool sent; the tool records
        /// it in the run report.
        model_identity: String,
    },
    /// Final verdict the harness returns after `verify_box` on the
    /// retained scenario instance.
    Verdict(Verdict),
}

/// Compact summary of the planned scenario. The harness reports it
/// twice: once on [`HarnessResponse::HelloAck`] and once on
/// [`HarnessResponse::Open`]. The tool uses the `Open` copy as the
/// authoritative scenario identity for the case.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScenarioSummary {
    /// Canonical `scenarios/<StructIdent>` identity.
    pub name: String,
    /// Scene path the author declared on the plan.
    pub scene: PathBuf,
    /// Authored plan duration in nanoseconds.
    pub duration_ns: u64,
    /// Authored step count.
    pub step_count: usize,
    /// Authored capture count.
    pub capture_count: usize,
}

/// Lifecycle-observed evidence the tool forwards to the harness.
/// The harness converts this into a typed `EvidenceCollector` and
/// seals it into a [`crate::scenario::results::ScenarioRun`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LifecycleReport {
    /// Supervisor-assigned execution identity.
    pub execution_id: String,
    /// Quantum the simulator probed for this run (must match the
    /// Program's quantum).
    pub quantum_ns: u64,
    /// Completed transitions the lifecycle observed.
    pub completed_steps: u64,
    /// Final observation cut was observed.
    pub final_observation_cut_observed: bool,
    /// Final capture drain was observed.
    pub final_capture_drain_observed: bool,
    /// Step outcomes the supervisor reported.
    #[serde(default)]
    pub step_outcomes: Vec<StepOutcomeRecord>,
    /// Capture records the supervisor drained.
    #[serde(default)]
    pub capture_records: Vec<CaptureRecordRef>,
    /// Command replies the supervisor accepted.
    #[serde(default)]
    pub command_replies: Vec<CommandReplyRef>,
    /// Native body sample the simulator emitted, if the plan asked
    /// for one. `None` means the plan did not request it; an empty
    /// `payload` means the lifecycle reported a blank body and the
    /// harness should still record it.
    #[serde(default)]
    pub native_body: Option<NativeBodyRef>,
}

/// One observed step outcome the harness records verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StepOutcomeRecord {
    pub label: String,
    pub outcome: StepOutcome,
}

/// One observed capture record the harness records verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaptureRecordRef {
    pub name: String,
    pub record: CaptureRecord,
}

/// One observed command reply the harness records verbatim.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CommandReplyRef {
    pub label: String,
    pub response_bytes: Vec<u8>,
}

/// One observed native body sample the harness records as a
/// `CaptureRecord::NativeBody` under the user's capture name.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct NativeBodyRef {
    pub capture_name: String,
    pub payload: Vec<u8>,
}

/// Final outcome the harness returns to the tool. The tool only
/// reports success when `passed == true`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Verdict {
    pub passed: bool,
    pub detail: Option<String>,
}

impl Verdict {
    /// A passing verdict.
    pub fn pass(detail: Option<String>) -> Self {
        Self {
            passed: true,
            detail,
        }
    }

    /// A failing verdict. The detail is included in the tool's
    /// failure report.
    pub fn fail(detail: Option<String>) -> Self {
        Self {
            passed: false,
            detail,
        }
    }
}

/// Encode a [`Program`] for the wire as a base64 string of its
/// canonical envelope bytes. The tool's harness-side mirror is
/// [`decode_program_envelope`].
pub fn encode_program_envelope(program: &Program) -> String {
    BASE64_ENGINE.encode(program.program_bytes())
}

/// Decode a base64-encoded program envelope back into a typed
/// [`Program`].
pub fn decode_program_envelope(b64: &str) -> Result<Program, String> {
    let bytes = BASE64_ENGINE
        .decode(b64.as_bytes())
        .map_err(|source| format!("program envelope base64: {source}"))?;
    Program::decode(&bytes).map_err(|source| format!("program decode: {source}"))
}
