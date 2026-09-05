//! Durable local world-session documents shared across adapter and client ownership.
//!
//! This module owns only the versioned serialized records and their pure
//! structural validation. Filesystem layout and permissions, registration
//! leases, process liveness, orphan recovery, cleanup execution, and retention
//! policy remain responsibilities of the concrete host and local client.

use std::collections::BTreeSet;
use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use super::state::WorldSessionState;
use super::{WorldMember, WorldMemberTerminal};
use crate::identity::{ExecutionId, ProducerId};
use crate::model::identity::WorldId;
use crate::model::world::{WorldDigest, WorldInstanceId, WorldProgress, WorldProvenance};
use crate::supervisor::api::simulation::SimulationEndReason;
use crate::version::FrameworkVersion;

/// Schema of one immutable live local-world locator.
pub const LOCAL_WORLD_REGISTRATION_SCHEMA: &str = "phoxal/local-world-registration/v0";

/// Schema of one durable world checkpoint.
pub const WORLD_CHECKPOINT_SCHEMA: &str = "phoxal/world-checkpoint/v0";

/// Schema of one complete terminal world summary.
pub const WORLD_TERMINAL_SUMMARY_SCHEMA: &str = "phoxal/world-terminal-summary/v0";

/// Schema of one terminal member record.
pub const WORLD_MEMBER_TERMINAL_SCHEMA: &str = "phoxal/world-member-terminal/v0";

/// A structurally invalid durable world-session document.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
#[error("{detail}")]
pub struct WorldSessionDocumentError {
    detail: String,
}

impl WorldSessionDocumentError {
    fn new(detail: impl Into<String>) -> Self {
        Self {
            detail: detail.into(),
        }
    }
}

/// An operating-system process identified across PID reuse.
#[derive(
    phoxal_macros::DescribeWire, Clone, Copy, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(deny_unknown_fields)]
pub struct ProcessIdentity {
    /// Process identifier assigned by the operating system.
    pub pid: u32,
    /// Process birth time reported as Unix seconds.
    pub started_at_unix_s: u64,
}

impl ProcessIdentity {
    /// Validate the process identity without consulting the process table.
    ///
    /// # Errors
    ///
    /// Returns an error when the operating system process identifier is zero.
    pub fn validate(self) -> Result<(), WorldSessionDocumentError> {
        if self.pid == 0 {
            return Err(WorldSessionDocumentError::new(
                "process identity PID must be positive",
            ));
        }
        Ok(())
    }
}

/// Exact native process-tree ownership retained for orphan convergence.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(deny_unknown_fields)]
pub struct NativeProcessIdentity {
    /// Direct native process identity.
    pub process: ProcessIdentity,
    /// Canonical executable used to validate the process before signalling it.
    pub executable: PathBuf,
    /// Owned Unix process group, when the platform supplies that primitive.
    pub process_group: Option<u32>,
}

impl NativeProcessIdentity {
    /// Validate identity fields without inspecting or signalling a process.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero PID, an empty executable, or a zero process
    /// group. Filesystem canonicality and platform ownership remain local checks.
    pub fn validate(&self) -> Result<(), WorldSessionDocumentError> {
        self.process.validate()?;
        if self.executable.as_os_str().is_empty() {
            return Err(WorldSessionDocumentError::new(
                "native process executable is empty",
            ));
        }
        if self.process_group == Some(0) {
            return Err(WorldSessionDocumentError::new(
                "native process group must be positive",
            ));
        }
        Ok(())
    }
}

/// Immutable world identity carried by a local registration.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(deny_unknown_fields)]
pub struct RegisteredWorld {
    /// Compiled world identity.
    pub id: WorldId,
    /// Digest of the canonical world bundle.
    pub digest: WorldDigest,
}

/// Immutable locator written while one local world host holds its lease.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(deny_unknown_fields)]
pub struct LocalWorldRegistration {
    /// Exact document schema.
    pub schema: String,
    /// Hosted world-session identity.
    pub instance: WorldInstanceId,
    /// Loopback endpoint of the typed world-session API.
    pub endpoint: String,
    /// Host process identity.
    pub process: ProcessIdentity,
    /// Framework train served by the host.
    pub framework: FrameworkVersion,
    /// Immutable compiled world identity.
    pub world: RegisteredWorld,
    /// Instance-relative lease filename.
    pub lease: String,
}

impl LocalWorldRegistration {
    /// Validate fields that do not require the lease file or process table.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported schema, a mismatched instance, an
    /// empty endpoint, or an invalid process identity.
    pub fn validate_structure(
        &self,
        expected_instance: WorldInstanceId,
    ) -> Result<(), WorldSessionDocumentError> {
        require_schema(
            "local world registration",
            &self.schema,
            LOCAL_WORLD_REGISTRATION_SCHEMA,
        )?;
        if self.instance != expected_instance {
            return Err(WorldSessionDocumentError::new(format!(
                "registration {expected_instance} claims world instance {}",
                self.instance
            )));
        }
        if self.endpoint.is_empty() {
            return Err(WorldSessionDocumentError::new(
                "world registration endpoint is empty",
            ));
        }
        self.process.validate()
    }
}

/// Last durable typed world state and process ownership written by the host.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorldCheckpoint {
    /// Exact document schema.
    pub schema: String,
    /// Host process identity.
    pub process: ProcessIdentity,
    /// Separately grouped native process tree, once launched.
    pub native_process: Option<NativeProcessIdentity>,
    /// Last complete public world state.
    pub state: WorldSessionState,
    /// Host wall-clock write time as Unix milliseconds.
    pub updated_at_unix_ms: u64,
}

impl WorldCheckpoint {
    /// Validate the checkpoint against its immutable live registration.
    ///
    /// # Errors
    ///
    /// Returns an error when either document is structurally inconsistent.
    /// Process liveness, executable canonicality, and process-group ownership
    /// require local platform checks and are deliberately outside this method.
    pub fn validate_structure(
        &self,
        registration: &LocalWorldRegistration,
    ) -> Result<(), WorldSessionDocumentError> {
        require_schema(
            "world checkpoint",
            &self.schema,
            WORLD_CHECKPOINT_SCHEMA,
        )?;
        if self.process != registration.process {
            return Err(WorldSessionDocumentError::new(format!(
                "world checkpoint process identity disagrees with registration for {}",
                registration.instance
            )));
        }
        validate_timestamp_after_process(
            self.updated_at_unix_ms,
            self.process,
            "world checkpoint predates registered host process birth",
        )?;
        if self.state.instance != registration.instance {
            return Err(WorldSessionDocumentError::new(format!(
                "world checkpoint instance {} disagrees with registration {}",
                self.state.instance, registration.instance
            )));
        }
        if self.state.provenance.framework != registration.framework
            || self.state.provenance.world != registration.world.id
            || self.state.provenance.digest != registration.world.digest
        {
            return Err(WorldSessionDocumentError::new(format!(
                "world checkpoint provenance disagrees with registration for {}",
                registration.instance
            )));
        }
        self.state.validate().map_err(|source| {
            WorldSessionDocumentError::new(format!("invalid checkpoint world state: {source}"))
        })?;
        if let Some(native) = &self.native_process {
            native.validate()?;
            validate_timestamp_after_process(
                self.updated_at_unix_ms,
                native.process,
                "world checkpoint predates its recorded native process birth",
            )?;
        }
        Ok(())
    }
}

/// One member-terminal artifact indexed by a world summary.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(deny_unknown_fields)]
pub struct WorldMemberEvidenceIndex {
    /// Execution whose terminal evidence is indexed.
    pub execution: ExecutionId,
    /// Session-relative path to the member record.
    pub path: String,
}

/// Persisted terminal evidence for one former member.
#[derive(Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorldMemberEvidence {
    /// Exact document schema.
    pub schema: String,
    /// Generic member-terminal payload flattened into the document root.
    #[serde(flatten)]
    pub terminal: WorldMemberTerminal,
}

// The derive deliberately rejects `serde(flatten)`. This record's serializer
// writes the schema field followed by the exact fields of WorldMemberTerminal.
impl crate::__compat::wire::DescribeWire for WorldMemberEvidence {
    fn wire_schema() -> crate::__compat::wire::WireSchema {
        use crate::__compat::wire::{WireField, WireSchema};

        WireSchema::structure([
            WireField::required("schema", String::wire_schema()),
            WireField::required("execution", ExecutionId::wire_schema()),
            WireField::required("robot", crate::identity::RobotId::wire_schema()),
            WireField::required("controller", ProducerId::wire_schema()),
            WireField::required("spawn", crate::model::identity::SpawnId::wire_schema()),
            WireField::required(
                "reason",
                super::WorldMemberEndReason::wire_schema(),
            ),
            WireField::required("last_progress", WorldProgress::wire_schema()),
            WireField::required("cleanup", super::WorldMemberCleanup::wire_schema()),
            WireField::required("evidence_paths", Vec::<String>::wire_schema()),
        ])
    }
}

impl WorldMemberEvidence {
    /// Validate the record wrapper against the execution named by its filename.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported schema or mismatched execution.
    pub fn validate_structure(
        &self,
        expected_execution: ExecutionId,
    ) -> Result<(), WorldSessionDocumentError> {
        require_schema(
            "member evidence",
            &self.schema,
            WORLD_MEMBER_TERMINAL_SCHEMA,
        )?;
        if self.terminal.execution != expected_execution {
            return Err(WorldSessionDocumentError::new(format!(
                "member evidence for {expected_execution} contains execution {}",
                self.terminal.execution
            )));
        }
        Ok(())
    }
}

/// Whether one world stopped orderly or failed.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum TerminalOutcome {
    /// Orderly world termination.
    Stopped {
        /// Typed stop reason.
        reason: SimulationEndReason,
    },
    /// Failed world termination.
    Failed {
        /// Typed failure reason.
        reason: SimulationEndReason,
        /// Human-readable evidence for the specific occurrence.
        detail: String,
    },
}

impl TerminalOutcome {
    /// Stable display category used by local clients.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Stopped { .. } => "stopped",
            Self::Failed { .. } => "failed",
        }
    }

    /// Typed reason carried by either outcome.
    #[must_use]
    pub const fn reason(&self) -> SimulationEndReason {
        match self {
            Self::Stopped { reason } | Self::Failed { reason, .. } => *reason,
        }
    }

    /// Occurrence detail for a failed outcome.
    #[must_use]
    pub fn detail(&self) -> Option<&str> {
        match self {
            Self::Stopped { .. } => None,
            Self::Failed { detail, .. } => Some(detail),
        }
    }

    /// Validate the relationship between outcome kind and end reason.
    ///
    /// # Errors
    ///
    /// Returns an error unless `WorldStopped` is carried exclusively by the
    /// orderly stopped outcome.
    pub fn validate(&self) -> Result<(), WorldSessionDocumentError> {
        match self {
            Self::Stopped {
                reason: SimulationEndReason::WorldStopped,
            } => Ok(()),
            Self::Stopped { reason } => Err(WorldSessionDocumentError::new(format!(
                "stopped terminal outcome cannot carry failure reason {reason:?}"
            ))),
            Self::Failed {
                reason: SimulationEndReason::WorldStopped,
                ..
            } => Err(WorldSessionDocumentError::new(
                "failed terminal outcome cannot carry WorldStopped",
            )),
            Self::Failed { detail, .. } if detail.is_empty() => Err(
                WorldSessionDocumentError::new("failed terminal outcome requires nonempty detail"),
            ),
            Self::Failed { .. } => Ok(()),
        }
    }
}

/// Process or producer attributed as the terminal failure source.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(deny_unknown_fields)]
pub struct TerminalFailure {
    /// Exact process identity, when process attribution is available.
    pub process: Option<ProcessIdentity>,
    /// Exact producer identity, when producer attribution is available.
    pub producer: Option<ProducerId>,
}

impl TerminalFailure {
    /// Validate any process attribution without consulting the process table.
    ///
    /// # Errors
    ///
    /// Returns an error when the attributed process has a zero PID.
    pub fn validate(&self) -> Result<(), WorldSessionDocumentError> {
        if let Some(process) = self.process {
            process.validate()?;
        }
        Ok(())
    }
}

/// Whether terminal cleanup removed every owned resource.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(deny_unknown_fields)]
pub struct TerminalCleanup {
    /// True only when cleanup converged without known residue.
    pub complete: bool,
    /// Cleanup failure detail when convergence was incomplete.
    pub detail: Option<String>,
}

impl TerminalCleanup {
    /// Validate that cleanup success and detail do not contradict each other.
    ///
    /// # Errors
    ///
    /// Returns an error when complete cleanup carries failure detail or
    /// incomplete cleanup does not carry nonempty detail.
    pub fn validate(&self) -> Result<(), WorldSessionDocumentError> {
        match (self.complete, self.detail.as_deref()) {
            (true, None) => Ok(()),
            (true, Some(_)) => Err(WorldSessionDocumentError::new(
                "complete terminal cleanup cannot carry failure detail",
            )),
            (false, Some(detail)) if !detail.is_empty() => Ok(()),
            (false, None | Some(_)) => Err(WorldSessionDocumentError::new(
                "incomplete terminal cleanup requires nonempty failure detail",
            )),
        }
    }
}

/// Bounded evidence-retention outcome for one terminal session.
#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, Deserialize, Eq, PartialEq, Serialize,
)]
#[serde(deny_unknown_fields)]
pub struct TerminalRetention {
    /// Total byte limit configured for retained logs.
    pub log_byte_limit: u64,
    /// Session-relative evidence files truncated at their bounds.
    pub truncated: Vec<String>,
}

impl TerminalRetention {
    /// Validate the bounded-retention accounting.
    ///
    /// # Errors
    ///
    /// Returns an error for a zero byte limit or duplicate truncated paths.
    pub fn validate(&self) -> Result<(), WorldSessionDocumentError> {
        if self.log_byte_limit == 0 {
            return Err(WorldSessionDocumentError::new(
                "terminal retention log byte limit must be positive",
            ));
        }
        let mut truncated = BTreeSet::new();
        for path in &self.truncated {
            if !truncated.insert(path) {
                return Err(WorldSessionDocumentError::new(format!(
                    "terminal retention lists truncated path `{path}` more than once"
                )));
            }
        }
        Ok(())
    }
}

/// Complete terminal projection written only after world cleanup converges.
#[derive(phoxal_macros::DescribeWire, Clone, Debug, Deserialize, PartialEq, Serialize)]
#[serde(deny_unknown_fields)]
pub struct WorldTerminalSummary {
    /// Exact document schema.
    pub schema: String,
    /// Terminal world-session identity.
    pub instance: WorldInstanceId,
    /// Immutable world provenance.
    pub provenance: WorldProvenance,
    /// Typed terminal outcome.
    pub outcome: TerminalOutcome,
    /// Last authoritative world progress.
    pub progress: WorldProgress,
    /// Last complete public member projection before cleanup.
    pub members: Vec<WorldMember>,
    /// Indexed per-member terminal artifacts.
    pub member_evidence: Vec<WorldMemberEvidenceIndex>,
    /// Best available failure attribution.
    pub failing: TerminalFailure,
    /// Session-relative world evidence paths.
    pub evidence: Vec<String>,
    /// Cleanup convergence result.
    pub cleanup: TerminalCleanup,
    /// Bounded-retention result.
    pub retention: TerminalRetention,
    /// Terminal write time as Unix milliseconds.
    pub ended_at_unix_ms: u64,
}

impl WorldTerminalSummary {
    /// Validate relationships contained entirely in the terminal document.
    ///
    /// # Errors
    ///
    /// Returns an error for an unsupported schema, a mismatched instance,
    /// invalid progress, duplicate member evidence, or a member-evidence path
    /// that disagrees with its execution. Filesystem path safety remains local.
    pub fn validate_structure(
        &self,
        expected_instance: WorldInstanceId,
    ) -> Result<(), WorldSessionDocumentError> {
        require_schema(
            "terminal world summary",
            &self.schema,
            WORLD_TERMINAL_SUMMARY_SCHEMA,
        )?;
        if self.instance != expected_instance {
            return Err(WorldSessionDocumentError::new(format!(
                "terminal summary for {expected_instance} contains instance {}",
                self.instance
            )));
        }
        self.progress
            .validate(self.provenance.time_step_ns)
            .map_err(|source| {
                WorldSessionDocumentError::new(format!(
                    "terminal world progress disagrees with retained provenance: {source}"
                ))
            })?;
        for member in &self.members {
            member
                .attached_at
                .world
                .validate(self.provenance.time_step_ns)
                .map_err(|source| {
                    WorldSessionDocumentError::new(format!(
                        "terminal member {} attachment disagrees with retained provenance: {source}",
                        member.execution
                    ))
                })?;
            if member.attached_at.world.completed_step() > self.progress.completed_step()
                || member.attached_at.world.elapsed_ns() > self.progress.elapsed_ns()
            {
                return Err(WorldSessionDocumentError::new(format!(
                    "terminal member {} attachment cannot be ahead of retained progress",
                    member.execution
                )));
            }
        }
        if self
            .members
            .windows(2)
            .any(|pair| pair[0].execution.to_string() >= pair[1].execution.to_string())
        {
            return Err(WorldSessionDocumentError::new(
                "terminal world members must be unique and ordered by ExecutionId",
            ));
        }
        let mut indexed_members = BTreeSet::new();
        for member in &self.member_evidence {
            let expected_path = format!("members/{}.json", member.execution);
            if member.path != expected_path {
                return Err(WorldSessionDocumentError::new(format!(
                    "member evidence path `{}` disagrees with execution {}",
                    member.path, member.execution
                )));
            }
            if !indexed_members.insert(member.execution.to_string()) {
                return Err(WorldSessionDocumentError::new(format!(
                    "member evidence indexes execution {} more than once",
                    member.execution
                )));
            }
        }
        self.outcome.validate()?;
        self.failing.validate()?;
        self.cleanup.validate()?;
        self.retention.validate()?;
        Ok(())
    }
}

fn require_schema(
    document: &str,
    actual: &str,
    expected: &'static str,
) -> Result<(), WorldSessionDocumentError> {
    if actual != expected {
        return Err(WorldSessionDocumentError::new(format!(
            "unsupported {document} schema `{actual}`"
        )));
    }
    Ok(())
}

fn validate_timestamp_after_process(
    timestamp_unix_ms: u64,
    process: ProcessIdentity,
    message: &'static str,
) -> Result<(), WorldSessionDocumentError> {
    let process_unix_ms = process
        .started_at_unix_s
        .checked_mul(1_000)
        .ok_or_else(|| WorldSessionDocumentError::new("process birth time overflows milliseconds"))?;
    if timestamp_unix_ms < process_unix_ms {
        return Err(WorldSessionDocumentError::new(message));
    }
    Ok(())
}

/// Compatibility records for the four schema-tagged persisted documents.
#[doc(hidden)]
pub mod __compat {
    use super::{
        LOCAL_WORLD_REGISTRATION_SCHEMA, LocalWorldRegistration, WORLD_CHECKPOINT_SCHEMA,
        WORLD_MEMBER_TERMINAL_SCHEMA, WORLD_TERMINAL_SUMMARY_SCHEMA, WorldCheckpoint,
        WorldMemberEvidence, WorldTerminalSummary,
    };
    use crate::__compat::surface::ContractRecord;
    use crate::__compat::wire::DescribeWire;

    pub(crate) fn contract_records(out: &mut Vec<ContractRecord>) {
        out.extend([
            ContractRecord::document(
                "LocalWorldRegistration",
                LOCAL_WORLD_REGISTRATION_SCHEMA,
                LocalWorldRegistration::wire_schema(),
            ),
            ContractRecord::document(
                "WorldCheckpoint",
                WORLD_CHECKPOINT_SCHEMA,
                WorldCheckpoint::wire_schema(),
            ),
            ContractRecord::document(
                "WorldMemberEvidence",
                WORLD_MEMBER_TERMINAL_SCHEMA,
                WorldMemberEvidence::wire_schema(),
            ),
            ContractRecord::document(
                "WorldTerminalSummary",
                WORLD_TERMINAL_SUMMARY_SCHEMA,
                WorldTerminalSummary::wire_schema(),
            ),
        ]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::__compat::wire::DescribeWire;
    use crate::bus::RobotInstant;
    use crate::identity::TimelineId;
    use crate::model::identity::{RobotId, SpawnId};
    use crate::model::world::LiveAttachmentBoundary;
    use crate::world::api::session::{
        WorldLifecycle, WorldMemberCleanup, WorldMemberEndReason, WorldMemberPhase,
    };

    fn execution() -> ExecutionId {
        ExecutionId::parse("10000000000000000000000000000001")
            .expect("canonical execution")
    }

    fn instance() -> WorldInstanceId {
        WorldInstanceId::parse("20000000000000000000000000000002")
            .expect("canonical world instance")
    }

    fn registration() -> LocalWorldRegistration {
        LocalWorldRegistration {
            schema: LOCAL_WORLD_REGISTRATION_SCHEMA.to_owned(),
            instance: instance(),
            endpoint: "tcp://127.0.0.1:7000".to_owned(),
            process: ProcessIdentity {
                pid: 42,
                started_at_unix_s: 99,
            },
            framework: FrameworkVersion::CURRENT,
            world: RegisteredWorld {
                id: WorldId::new("warehouse").expect("world id"),
                digest: WorldDigest::parse(&"aa".repeat(32)).expect("world digest"),
            },
            lease: format!("{}.lease", instance()),
        }
    }

    fn provenance() -> WorldProvenance {
        WorldProvenance {
            world: WorldId::new("warehouse").expect("world id"),
            digest: WorldDigest::parse(&"aa".repeat(32)).expect("world digest"),
            random_seed: 7,
            framework: FrameworkVersion::CURRENT,
            adapter: "webots".to_owned(),
            adapter_version: "0.68.0".to_owned(),
            simulator_version: "R2025a".to_owned(),
            platform: "test".to_owned(),
            time_step_ns: 12_000_000,
        }
    }

    fn member(execution: &str, producer: u128, attached_step: u64) -> WorldMember {
        WorldMember {
            execution: ExecutionId::parse(execution).expect("canonical execution"),
            robot: RobotId::new("rover").expect("robot id"),
            controller: ProducerId::try_from(producer).expect("producer"),
            phase: WorldMemberPhase::Active,
            attached_at: LiveAttachmentBoundary {
                world: WorldProgress::at(attached_step, 12_000_000)
                    .expect("attachment progress"),
                execution: RobotInstant::new(
                    TimelineId::from_raw(1).expect("timeline"),
                    attached_step,
                ),
            },
            spawn: SpawnId::new("bay").expect("spawn"),
            initial_pose: serde_json::from_value(serde_json::json!({
                "xyz": [0.0, 0.0, 0.0],
                "rpy": [0.0, 0.0, 0.0]
            }))
            .expect("pose"),
        }
    }

    fn summary(members: Vec<WorldMember>, completed_step: u64) -> WorldTerminalSummary {
        WorldTerminalSummary {
            schema: WORLD_TERMINAL_SUMMARY_SCHEMA.to_owned(),
            instance: instance(),
            provenance: provenance(),
            outcome: TerminalOutcome::Stopped {
                reason: SimulationEndReason::WorldStopped,
            },
            progress: WorldProgress::at(completed_step, 12_000_000).expect("world progress"),
            members,
            member_evidence: Vec::new(),
            failing: TerminalFailure {
                process: None,
                producer: None,
            },
            evidence: vec!["host.log".to_owned(), "webots.log".to_owned()],
            cleanup: TerminalCleanup {
                complete: true,
                detail: None,
            },
            retention: TerminalRetention {
                log_byte_limit: 1024,
                truncated: Vec::new(),
            },
            ended_at_unix_ms: 100_000,
        }
    }

    #[test]
    fn registration_and_process_identity_keep_the_exact_v0_shape() {
        let registration = registration();
        registration
            .validate_structure(instance())
            .expect("registration validates");
        let value = serde_json::to_value(&registration).expect("registration encodes");
        assert_eq!(
            LocalWorldRegistration::wire_schema().conforms(&value),
            Ok(())
        );
        assert_eq!(value["schema"], LOCAL_WORLD_REGISTRATION_SCHEMA);
        assert_eq!(value["process"]["started_at_unix_s"], 99);
        assert!(value.get("controller_endpoint").is_none());
        assert_eq!(
            serde_json::from_value::<LocalWorldRegistration>(value)
                .expect("registration decodes"),
            registration
        );
    }

    #[test]
    fn registration_validation_rejects_a_zero_process_and_wrong_instance() {
        let mut registration = registration();
        registration.process.pid = 0;
        assert_eq!(
            registration
                .validate_structure(instance())
                .expect_err("zero PID is rejected")
                .to_string(),
            "process identity PID must be positive"
        );
        registration.process.pid = 42;
        let other = WorldInstanceId::parse("30000000000000000000000000000003")
            .expect("other world instance");
        assert!(
            registration
                .validate_structure(other)
                .expect_err("wrong instance is rejected")
                .to_string()
                .contains("claims world instance")
        );
    }

    #[test]
    fn checkpoint_round_trips_and_validates_against_registration() {
        let registration = registration();
        let checkpoint = WorldCheckpoint {
            schema: WORLD_CHECKPOINT_SCHEMA.to_owned(),
            process: registration.process,
            native_process: Some(NativeProcessIdentity {
                process: ProcessIdentity {
                    pid: 43,
                    started_at_unix_s: 99,
                },
                executable: PathBuf::from("/Webots"),
                process_group: Some(43),
            }),
            state: WorldSessionState {
                revision: 0,
                instance: instance(),
                provenance: WorldProvenance {
                    framework: registration.framework,
                    world: registration.world.id.clone(),
                    digest: registration.world.digest,
                    ..provenance()
                },
                lifecycle: WorldLifecycle::Starting,
                progress: WorldProgress::zero(12_000_000).expect("zero progress"),
                members: Vec::new(),
            },
            updated_at_unix_ms: 100_000,
        };
        checkpoint
            .validate_structure(&registration)
            .expect("checkpoint validates");
        let value = serde_json::to_value(&checkpoint).expect("checkpoint encodes");
        assert_eq!(WorldCheckpoint::wire_schema().conforms(&value), Ok(()));
        assert_eq!(value["schema"], WORLD_CHECKPOINT_SCHEMA);
        assert_eq!(
            serde_json::from_value::<WorldCheckpoint>(value).expect("checkpoint decodes"),
            checkpoint
        );
    }

    #[test]
    fn member_evidence_flattens_and_round_trips_the_generic_terminal_payload() {
        let member = WorldMemberEvidence {
            schema: WORLD_MEMBER_TERMINAL_SCHEMA.to_owned(),
            terminal: WorldMemberTerminal {
                execution: execution(),
                robot: RobotId::new("rover").expect("robot id"),
                controller: ProducerId::try_from(
                    0x3000_0000_0000_0000_0000_0000_0000_0003,
                )
                .expect("producer"),
                spawn: SpawnId::new("bay").expect("spawn"),
                reason: WorldMemberEndReason::Stopped,
                last_progress: WorldProgress::zero(12_000_000).expect("progress"),
                cleanup: WorldMemberCleanup::Complete,
                evidence_paths: vec![format!("members/{}.actuation.json", execution())],
            },
        };
        member
            .validate_structure(execution())
            .expect("member evidence validates");
        let value = serde_json::to_value(&member).expect("member evidence encodes");
        assert_eq!(WorldMemberEvidence::wire_schema().conforms(&value), Ok(()));
        assert_eq!(value["schema"], WORLD_MEMBER_TERMINAL_SCHEMA);
        assert_eq!(value["execution"], execution().to_string());
        assert!(value.get("terminal").is_none());
        assert_eq!(
            serde_json::from_value::<WorldMemberEvidence>(value)
                .expect("member evidence decodes"),
            member
        );
    }

    #[test]
    fn terminal_outcome_helpers_preserve_the_tagged_shape() {
        let outcome = TerminalOutcome::Failed {
            reason: SimulationEndReason::HostLost,
            detail: "host disappeared".to_owned(),
        };
        assert_eq!(outcome.kind(), "failed");
        assert_eq!(outcome.reason(), SimulationEndReason::HostLost);
        assert_eq!(outcome.detail(), Some("host disappeared"));
        assert_eq!(
            serde_json::to_value(outcome).expect("outcome encodes"),
            serde_json::json!({
                "kind": "failed",
                "reason": "host_lost",
                "detail": "host disappeared"
            })
        );
    }

    #[test]
    fn terminal_outcome_cleanup_and_retention_reject_contradictions() {
        assert!(
            TerminalOutcome::Stopped {
                reason: SimulationEndReason::HostLost,
            }
            .validate()
            .is_err()
        );
        assert!(
            TerminalOutcome::Failed {
                reason: SimulationEndReason::WorldStopped,
                detail: "contradiction".to_owned(),
            }
            .validate()
            .is_err()
        );
        assert!(
            TerminalOutcome::Failed {
                reason: SimulationEndReason::HostLost,
                detail: String::new(),
            }
            .validate()
            .is_err()
        );
        assert!(
            TerminalCleanup {
                complete: true,
                detail: Some("not complete".to_owned()),
            }
            .validate()
            .is_err()
        );
        assert!(
            TerminalCleanup {
                complete: false,
                detail: Some(String::new()),
            }
            .validate()
            .is_err()
        );
        assert!(
            TerminalRetention {
                log_byte_limit: 0,
                truncated: Vec::new(),
            }
            .validate()
            .is_err()
        );
        assert!(
            TerminalRetention {
                log_byte_limit: 1,
                truncated: vec!["host.log".to_owned(), "host.log".to_owned()],
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn terminal_summary_requires_ordered_members_with_past_attachment_boundaries() {
        let first = member(
            "10000000000000000000000000000001",
            0x3000_0000_0000_0000_0000_0000_0000_0003,
            1,
        );
        let second = member(
            "20000000000000000000000000000002",
            0x4000_0000_0000_0000_0000_0000_0000_0004,
            2,
        );

        let valid = summary(vec![first.clone(), second.clone()], 2);
        valid
            .validate_structure(instance())
            .expect("ordered terminal summary validates");
        let value = serde_json::to_value(&valid).expect("terminal summary encodes");
        assert_eq!(WorldTerminalSummary::wire_schema().conforms(&value), Ok(()));
        assert_eq!(value["schema"], WORLD_TERMINAL_SUMMARY_SCHEMA);
        assert_eq!(
            serde_json::from_value::<WorldTerminalSummary>(value)
                .expect("terminal summary decodes"),
            valid
        );

        assert!(
            summary(vec![second.clone(), first.clone()], 2)
                .validate_structure(instance())
                .is_err()
        );
        assert!(
            summary(vec![first.clone(), first], 2)
                .validate_structure(instance())
                .is_err()
        );
        assert!(
            summary(vec![second], 1)
                .validate_structure(instance())
                .is_err()
        );
    }
}
