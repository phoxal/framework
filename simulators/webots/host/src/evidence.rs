//! Durable owner-only world-session evidence.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::Mutex;

use anyhow::{Context, Result, ensure};
use phoxal::identity::ExecutionId;
use phoxal::model::world::{WorldBundle, WorldInstanceId, WorldProgress, WorldProvenance};
use phoxal::world::api::session::document::{
    ProcessIdentity, TerminalCleanup, TerminalFailure, TerminalOutcome, TerminalRetention,
    WORLD_CHECKPOINT_SCHEMA, WORLD_MEMBER_TERMINAL_SCHEMA, WORLD_TERMINAL_SUMMARY_SCHEMA,
    WorldCheckpoint, WorldMemberEvidence, WorldMemberEvidenceIndex, WorldTerminalSummary,
};
use phoxal::world::api::session::state::WorldSessionState;
use phoxal::world::api::session::{WorldMember, WorldMemberTerminal};
use serde::{Deserialize, Serialize};

use crate::lifecycle::NativeProcessIdentity;

const ACTUATION_SCHEMA: &str = "phoxal/world-member-actuation/v0";

/// Owner of one retained evidence directory.
#[derive(Debug)]
pub struct EvidenceSession {
    root: PathBuf,
    writes: Mutex<()>,
    native_process: Mutex<Option<NativeProcessIdentity>>,
}

impl EvidenceSession {
    /// Create the session directory and persist the canonical bundle before readiness.
    pub fn create(
        evidence_root: impl AsRef<Path>,
        instance: WorldInstanceId,
        bundle: &WorldBundle,
        log_byte_limit: u64,
    ) -> Result<Self> {
        ensure!(
            log_byte_limit > 0,
            "simulation log byte limit must be positive"
        );
        let evidence_root = evidence_root.as_ref().canonicalize().with_context(|| {
            format!(
                "failed to open evidence root {}",
                evidence_root.as_ref().display()
            )
        })?;
        secure_directory(&evidence_root)?;
        let root = evidence_root.join(instance.to_string());
        create_owner_directory(&root)?;
        create_owner_directory(&root.join("members"))?;
        bundle
            .write(root.join("world-bundle"))
            .context("failed to retain the canonical world bundle")?;
        Ok(Self {
            root,
            writes: Mutex::new(()),
            native_process: Mutex::new(None),
        })
    }

    /// Record the separately grouped Webots process tree immediately after launch.
    pub fn set_native_process(&self, identity: NativeProcessIdentity) {
        *lock(&self.native_process) = Some(identity);
    }

    #[must_use]
    pub fn native_process(&self) -> Option<NativeProcessIdentity> {
        lock(&self.native_process).clone()
    }

    #[must_use]
    pub fn webots_log(&self) -> PathBuf {
        self.root.join("webots.log")
    }

    /// Atomically retain one member-terminal record.
    pub fn write_member(&self, member: &WorldMemberEvidence) -> Result<()> {
        member.validate_structure(member.terminal.execution)?;
        let _write = lock(&self.writes);
        atomic_owner_json(
            &self
                .root
                .join("members")
                .join(format!("{}.json", member.terminal.execution)),
            member,
        )
    }

    /// Persist the bounded typed applied-action record for one terminal member.
    pub fn write_actuation(
        &self,
        execution: ExecutionId,
        records: Vec<phoxal_simulator_webots_shared::protocol::ActuationEvidence>,
        dropped_records: u64,
    ) -> Result<String> {
        let relative = format!("members/{execution}.actuation.json");
        let retained_records = u64::try_from(records.len())
            .context("retained applied-action record count does not fit in u64")?;
        let document = MemberActuationEvidence {
            schema: ACTUATION_SCHEMA.to_owned(),
            execution,
            retention: ActuationRetention {
                retained_records,
                dropped_records,
            },
            records,
        };
        let _write = lock(&self.writes);
        atomic_owner_json(&self.root.join(&relative), &document)?;
        Ok(relative)
    }

    /// Atomically retain the last host identity and typed public world state.
    pub fn write_checkpoint(&self, checkpoint: &WorldCheckpoint) -> Result<()> {
        ensure!(
            checkpoint.schema == WORLD_CHECKPOINT_SCHEMA,
            "invalid world checkpoint schema"
        );
        let _write = lock(&self.writes);
        atomic_owner_json(&self.root.join("checkpoint.json"), checkpoint)
    }

    /// Enumerate the immutable member records retained by this session.
    pub fn member_evidence(&self) -> Result<Vec<WorldMemberEvidenceIndex>> {
        let mut evidence = Vec::new();
        for entry in std::fs::read_dir(self.root.join("members"))? {
            let entry = entry?;
            let path = entry.path();
            if path.extension().and_then(std::ffi::OsStr::to_str) != Some("json") {
                continue;
            }
            let stem = path
                .file_stem()
                .and_then(std::ffi::OsStr::to_str)
                .context("member evidence filename is not UTF-8")?;
            if stem.ends_with(".actuation") {
                continue;
            }
            let execution = ExecutionId::parse(stem)
                .context("member evidence filename is not an ExecutionId")?;
            evidence.push(WorldMemberEvidenceIndex {
                execution,
                path: format!("members/{execution}.json"),
            });
        }
        evidence.sort_by_key(|item| item.execution.to_string());
        Ok(evidence)
    }

    /// Write `summary.json` last. Its presence is the complete terminal marker.
    pub fn write_summary(&self, summary: &WorldTerminalSummary) -> Result<()> {
        summary.validate_structure(summary.instance)?;
        let _write = lock(&self.writes);
        atomic_owner_json(&self.root.join("summary.json"), summary)
    }
}

#[must_use]
pub fn world_checkpoint(
    process: ProcessIdentity,
    native_process: Option<NativeProcessIdentity>,
    state: WorldSessionState,
) -> WorldCheckpoint {
    WorldCheckpoint {
        schema: WORLD_CHECKPOINT_SCHEMA.to_owned(),
        process,
        native_process,
        state,
        updated_at_unix_ms: unix_ms(),
    }
}

#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn world_terminal_summary(
    instance: WorldInstanceId,
    provenance: WorldProvenance,
    outcome: TerminalOutcome,
    progress: WorldProgress,
    members: Vec<WorldMember>,
    member_evidence: Vec<WorldMemberEvidenceIndex>,
    failing: TerminalFailure,
    cleanup: TerminalCleanup,
    retention: TerminalRetention,
) -> WorldTerminalSummary {
    WorldTerminalSummary {
        schema: WORLD_TERMINAL_SUMMARY_SCHEMA.to_owned(),
        instance,
        provenance,
        outcome,
        progress,
        members,
        member_evidence,
        failing,
        evidence: vec!["host.log".to_owned(), "webots.log".to_owned()],
        cleanup,
        retention,
        ended_at_unix_ms: unix_ms(),
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MemberActuationEvidence {
    pub schema: String,
    pub execution: ExecutionId,
    pub retention: ActuationRetention,
    pub records: Vec<phoxal_simulator_webots_shared::protocol::ActuationEvidence>,
}

/// Exact bounded-retention accounting for one applied-action artifact.
#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ActuationRetention {
    pub retained_records: u64,
    pub dropped_records: u64,
}

#[must_use]
pub fn world_member_evidence(terminal: WorldMemberTerminal) -> WorldMemberEvidence {
    WorldMemberEvidence {
        schema: WORLD_MEMBER_TERMINAL_SCHEMA.to_owned(),
        terminal,
    }
}

fn atomic_owner_json(path: &Path, value: &impl Serialize) -> Result<()> {
    let parent = path.parent().context("evidence path has no parent")?;
    let temporary = parent.join(format!(
        ".{}-{}.tmp",
        path.file_name()
            .and_then(std::ffi::OsStr::to_str)
            .unwrap_or("evidence"),
        std::process::id()
    ));
    let result = (|| -> Result<()> {
        let mut file = owner_file(&temporary)?;
        serde_json::to_writer(&mut file, value)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        std::fs::rename(&temporary, path)?;
        File::open(parent)?.sync_all()?;
        Ok(())
    })();
    if result.is_err() {
        let _ = std::fs::remove_file(&temporary);
    }
    result
}

fn owner_file(path: &Path) -> Result<File> {
    let mut options = OpenOptions::new();
    options.create_new(true).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    Ok(options.open(path)?)
}

fn create_owner_directory(path: &Path) -> Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt as _;
        let mut builder = std::fs::DirBuilder::new();
        builder.mode(0o700);
        builder.create(path)?;
    }
    #[cfg(not(unix))]
    std::fs::create_dir(path)?;
    Ok(())
}

fn secure_directory(path: &Path) -> Result<()> {
    let metadata = std::fs::symlink_metadata(path)?;
    ensure!(metadata.is_dir(), "evidence root is not a directory");
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt as _;
        // SAFETY: `geteuid` has no pointer arguments or side effects.
        ensure!(
            metadata.uid() == unsafe { libc::geteuid() },
            "evidence root is owned by another user"
        );
        ensure!(
            metadata.mode() & 0o077 == 0,
            "evidence root must have mode 0700 or stricter"
        );
    }
    Ok(())
}

fn unix_ms() -> u64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |duration| {
            u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
        })
}

fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::model::identity::WorldId;
    use phoxal::version::FrameworkVersion;
    use phoxal::world::api::session::WorldLifecycle;

    #[test]
    fn summary_requires_both_failing_fields_even_when_absent() {
        let failing = TerminalFailure {
            process: None,
            producer: None,
        };
        assert_eq!(
            serde_json::to_value(failing).expect("failing identity encodes"),
            serde_json::json!({"process": null, "producer": null})
        );
    }

    #[test]
    fn actuation_artifacts_disclose_exact_bounded_retention() {
        let execution =
            ExecutionId::parse("10000000000000000000000000000001").expect("canonical execution");
        let directory = tempfile::tempdir().expect("temporary evidence directory");
        std::fs::create_dir(directory.path().join("members")).expect("member evidence directory");
        let evidence = EvidenceSession {
            root: directory.path().to_path_buf(),
            writes: Mutex::new(()),
            native_process: Mutex::new(None),
        };

        let relative = evidence
            .write_actuation(execution, Vec::new(), 19)
            .expect("actuation evidence writes");
        let encoded: serde_json::Value = serde_json::from_slice(
            &std::fs::read(directory.path().join(relative)).expect("actuation evidence bytes"),
        )
        .expect("actuation evidence decodes");
        assert_eq!(
            encoded["retention"],
            serde_json::json!({"retained_records": 0, "dropped_records": 19})
        );
    }

    #[test]
    fn checkpoint_atomically_replaces_owner_only_typed_process_state() {
        let instance = WorldInstanceId::parse("10000000000000000000000000000001")
            .expect("canonical world instance");
        let state = WorldSessionState {
            revision: 0,
            instance,
            provenance: WorldProvenance {
                world: WorldId::new("warehouse").expect("world id"),
                digest: phoxal::model::world::WorldDigest::parse(&"0".repeat(64)).expect("digest"),
                random_seed: 0,
                framework: FrameworkVersion::CURRENT,
                adapter: "webots".to_owned(),
                adapter_version: env!("CARGO_PKG_VERSION").to_owned(),
                simulator_version: "R2025a".to_owned(),
                platform: "test".to_owned(),
                time_step_ns: 12_000_000,
            },
            lifecycle: WorldLifecycle::Starting,
            progress: WorldProgress::zero(12_000_000).expect("zero progress"),
            members: Vec::new(),
        };
        let native = NativeProcessIdentity {
            process: ProcessIdentity {
                pid: 123,
                started_at_unix_s: 456,
            },
            executable: PathBuf::from("/Applications/Webots.app/Contents/MacOS/webots"),
            process_group: Some(123),
        };
        let mut checkpoint = world_checkpoint(
            ProcessIdentity {
                pid: 42,
                started_at_unix_s: 99,
            },
            Some(native.clone()),
            state,
        );
        let directory = tempfile::tempdir().expect("temporary evidence directory");
        let path = directory.path().join("checkpoint.json");
        atomic_owner_json(&path, &checkpoint).expect("initial checkpoint");
        checkpoint.state.revision = 1;
        atomic_owner_json(&path, &checkpoint).expect("replacement checkpoint");
        let observed: WorldCheckpoint =
            serde_json::from_slice(&std::fs::read(&path).expect("checkpoint bytes"))
                .expect("typed checkpoint");
        assert_eq!(observed.state.revision, 1);
        assert_eq!(observed.native_process, Some(native));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt as _;
            assert_eq!(
                std::fs::metadata(path).expect("metadata").mode() & 0o777,
                0o600
            );
        }
    }
}
