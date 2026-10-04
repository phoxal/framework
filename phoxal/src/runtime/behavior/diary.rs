//! Bounded accepted diagnostics for owned behavior trees.
//!
//! One [`BehaviorDiary`] is owned by each generated runtime adapter, exactly
//! like its pending-call ledger and capture registry. Every tree tick or
//! cancellation stages one compact record with the invocation candidate,
//! keyed by the tree's construction generation: distinct trees in one
//! candidate each keep their own record, while re-ticking or cancelling one
//! tree replaces only that tree's snapshot. The whole candidate collection
//! commits at the owning boundary's acceptance (the adapter's `accepted`
//! hook driven by the runtime owner) and is discarded wholesale
//! with a rejected, errored, or panicking candidate.
//!
//! # Bounds and loss
//!
//! Every record's serialized representation — the exact bytes an observer
//! receives, including JSON escaping and envelope fields — is produced and
//! checked against [`MAX_DIARY_BYTES`] **before** it is retained, whether it
//! is a new record or a replacement of the same tree's snapshot. An
//! obviously oversized raw path is refused by length before any
//! serialization work, and the serializer itself writes through a
//! budget-capped writer, so rejecting even an absurd caller-owned path
//! never allocates beyond the bound. A replacement that would exceed the
//! bound is refused by dropping that tree's staged diagnostic entirely: a
//! visible loss is safer than retaining a stale running snapshot that
//! would publish as if it were the accepted cancellation. The accepted
//! ring, the candidate collection, and the observation outbox are each
//! bounded; any refusal — ring, candidate, outbox, or a caught observer
//! panic — surfaces through one sticky [`BehaviorDiary::overflowed`] flag.
//! A successful queue handoff is not evidence of delivery, and a
//! subscriber that filters the target out is a disabled observer, not a
//! loss.
//!
//! # Observation facility
//!
//! Promotion never runs observer code: it retains the bounded ring and
//! hands each record's recorded representation to one process-global
//! bounded outbox consumed by a single observation worker, through a
//! nonblocking `try_send`. The worker replays records through the
//! dispatcher captured at acceptance using ordinary `tracing` emission
//! under that captured dispatcher, so an ordinary `phoxal::boundary`
//! subscriber observes accepted behavior records. The facility is owned
//! outside every adapter: it starts lazily on the first accepted record,
//! is bounded to one thread and one queue for the whole process, and is
//! never joined — adapter teardown is independent of arbitrary observer
//! code. A blocked observer callback can stall the facility (later
//! records refuse at the outbox bound and surface as visible loss); it
//! can never stall acceptance, behavior, or teardown. The facility's
//! shutdown boundary is process exit.
//!
//! # Reset fencing
//!
//! Reinitialization clears the diary and advances a diagnostic epoch
//! shared with the observation facility, exactly like the adapter's calls
//! and captures. The epoch and its loss bit have ONE owner — a short
//! mutex — so advancing the epoch/clearing loss and attributing a
//! delivery loss are totally ordered: a failure belonging to an old
//! generation can never mark the fresh one, no matter how the worker and
//! the owner interleave. Queued records of an old generation are dropped
//! before any subscriber callback runs. A record already inside a
//! subscriber callback when reset occurs cannot be cancelled — Rust
//! cannot force an arbitrary blocked callback to return — so the
//! supported contract is narrower there: that in-flight record still
//! completes delivery, but it carries its explicit pre-reset `epoch` in
//! the payload, so an observer can always distinguish it from
//! new-generation progress.

use std::collections::VecDeque;
use std::io::{self, Write};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::OnceLock;
use std::sync::mpsc::{Receiver, SyncSender, TrySendError, sync_channel};
use tracing::Dispatch;
use tracing::dispatcher;

use super::{FailureKind, TreeStatus};

/// Upper bound of retained accepted records per runtime.
pub const MAX_DIARY_RECORDS: usize = 64;
/// Upper bound of one record's serialized representation — retained and
/// emitted bytes, including JSON escaping and the envelope. Every emitted
/// event is strictly shorter than this, and refusing a larger input never
/// allocates more than this.
pub const MAX_DIARY_BYTES: usize = 16 * 1024;
/// Bound of the process-global observation outbox. Producers never block
/// on it: a full outbox refuses the record with a visible loss flag.
pub const MAX_OUTBOX_RECORDS: usize = 256;

/// One compact accepted diagnostic record for a ticked tree. Structural
/// facts only: no domain payload is copied and the path is bounded by the
/// depth limit.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct BehaviorRecord {
    /// The tree's globally unique construction generation.
    pub generation: u64,
    /// The invocation index this record was staged for.
    pub invocation: u64,
    /// The execution time of that invocation, in nanoseconds.
    pub time_ns: u64,
    /// The tree's status after this tick.
    pub status: TreeStatus,
    /// The typed failure classification of the terminal outcome.
    pub kind: FailureKind,
    /// The stable structural path of the active node after this tick.
    pub path: String,
    /// The adapter's outstanding generated calls (bounded ledger size).
    pub pending_calls: usize,
    /// The adapter's active typed captures.
    pub active_captures: usize,
}

/// The diagnostic generation fence shared between one diary and the
/// observation facility. The epoch and its loss bit have exactly one
/// owner — this short mutex — so reset (advance epoch, clear loss) and
/// loss attribution (mark only for the still-current epoch) are totally
/// ordered: the interleaving where an old job passes its epoch check,
/// reset completes, and the old job then stores loss can no longer mark
/// the fresh generation. The mutex is never held across a subscriber
/// callback, formatting, or queue waiting.
#[derive(Debug, Default)]
struct LossFence {
    state: Mutex<FenceState>,
}

#[derive(Debug, Default)]
struct FenceState {
    epoch: u64,
    /// Visible delivery loss observed by the observation facility for the
    /// current epoch (a caught observer panic). Cleared with the epoch.
    loss: bool,
}

impl LossFence {
    /// The current diagnostic epoch.
    fn epoch(&self) -> u64 {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).epoch
    }

    /// Whether delivery loss is recorded for the current epoch.
    fn loss(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).loss
    }

    /// Records delivery loss for `job_epoch` unless reset has advanced the
    /// epoch past it. The linearization point is this critical section:
    /// if a reset interleaves, either the reset ran first — its epoch no
    /// longer matches and nothing is marked — or the mark ran first and
    /// the reset clears it with the epoch. Either ordering leaves a fresh
    /// generation unmarked by old failures.
    fn mark_loss(&self, job_epoch: u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.epoch == job_epoch {
            state.loss = true;
        }
    }

    /// Advances the diagnostic epoch and clears its loss bit as one step:
    /// the linearization point of a reset for loss-attribution purposes.
    fn advance(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.epoch += 1;
        state.loss = false;
    }
}

/// One runtime's staged and accepted behavior diagnostics.
///
/// Lock ordering: the fence mutex and the diary-table mutex are never
/// held simultaneously — every method releases one before acquiring the
/// other — so no ordering cycle between them can exist.
#[derive(Debug, Default)]
pub struct BehaviorDiary {
    table: Mutex<DiaryTable>,
    fence: Arc<LossFence>,
}

/// One staged record with its recorded bounded encoding: the exact bytes
/// an observer receives, produced once at staging under the diagnostic
/// epoch it was staged in, and reused for retention accounting and
/// emission without re-serializing.
#[derive(Debug)]
struct StagedRecord {
    record: BehaviorRecord,
    encoded: String,
    epoch: u64,
}

#[derive(Debug, Default)]
struct DiaryTable {
    /// The current candidate's records, at most one per tree generation.
    staged: Vec<StagedRecord>,
    /// Total serialized bytes of the staged candidate collection.
    staged_bytes: usize,
    /// Whether the candidate collection refused at least one record.
    staged_loss: bool,
    /// The bounded ring of accepted records, oldest first.
    accepted: VecDeque<BehaviorRecord>,
    /// Total serialized bytes retained in the ring.
    bytes: usize,
    /// Sticky local loss: the ring or the candidate collection refused at
    /// least one record, or the outbox refused a handoff.
    overflow: bool,
}

/// One accepted record handed to the observation facility: the dispatcher
/// current at acceptance (a plain clone — no subscriber callback ran on
/// the acceptance path), the record's recorded bounded payload, the
/// diagnostic epoch it was staged in, and the diary's loss fence.
struct OutboxJob {
    dispatch: Dispatch,
    payload: String,
    epoch: u64,
    fence: Arc<LossFence>,
}

/// The process-global observation outbox: started lazily on the first
/// accepted record, one bounded queue and one worker thread for the whole
/// process, never joined.
static OUTBOX: OnceLock<SyncSender<OutboxJob>> = OnceLock::new();

impl BehaviorDiary {
    /// Stages one record with the invocation candidate. A tree ticked or
    /// cancelled repeatedly within one candidate replaces only its own
    /// snapshot; a distinct tree keeps its own record. The record's
    /// serialized size is produced under a byte budget and checked
    /// BEFORE storage: an obviously oversized raw path is refused by
    /// length before any serialization work, and a record whose encoding
    /// would exceed the bound is refused by the capped serializer itself.
    /// A refused record — or a refused replacement, which drops the
    /// tree's staged snapshot rather than retaining a stale predecessor —
    /// surfaces as visible loss and never faults behavior.
    pub fn stage(&self, record: BehaviorRecord) {
        // The serialized envelope is at least as long as the path itself,
        // so an oversized path is refused before any serialization
        // allocation at all.
        if record.path.len() >= MAX_DIARY_BYTES {
            self.refuse_staged(record.generation);
            return;
        }
        let epoch = self.fence.epoch();
        // Serialize once through the budget-capped writer: this is the
        // single recorded representation reused for retention accounting
        // and emission, and it can never allocate past the bound.
        let Some(encoded) = encode_bounded(&record, epoch) else {
            self.refuse_staged(record.generation);
            return;
        };
        let proposed = encoded.len();
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = table
            .staged
            .iter()
            .position(|staged| staged.record.generation == record.generation)
        {
            // Checked accounting: release the old charge first, then
            // compare the proposed charge against the remaining bound
            // before any storage change.
            let old = table.staged[index].encoded.len();
            let Some(remaining) = table.staged_bytes.checked_sub(old) else {
                table.staged_loss = true;
                return;
            };
            if proposed >= MAX_DIARY_BYTES || remaining + proposed > MAX_DIARY_BYTES {
                // Refused replacement: dropping the tree's diagnostic
                // with visible loss is safer than retaining a stale
                // running snapshot that would publish as if it were the
                // accepted cancellation.
                table.staged.remove(index);
                table.staged_bytes = remaining;
                table.staged_loss = true;
                return;
            }
            table.staged_bytes = remaining + proposed;
            table.staged[index] = StagedRecord {
                record,
                encoded,
                epoch,
            };
            return;
        }
        if table.staged.len() >= MAX_DIARY_RECORDS
            || proposed >= MAX_DIARY_BYTES
            || table.staged_bytes + proposed > MAX_DIARY_BYTES
        {
            table.staged_loss = true;
            return;
        }
        table.staged_bytes += proposed;
        table.staged.push(StagedRecord {
            record,
            encoded,
            epoch,
        });
    }

    fn refuse_staged(&self, generation: u64) {
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(index) = table
            .staged
            .iter()
            .position(|staged| staged.record.generation == generation)
        {
            let old = table.staged.remove(index);
            table.staged_bytes -= old.encoded.len();
        }
        table.staged_loss = true;
    }

    /// Publishes the whole staged collection as accepted: the owning
    /// boundary accepted the candidate. Records enter the bounded accepted
    /// ring — when the ring is full the NEW record is dropped and the
    /// sticky overflow flag becomes visible, never silently coalesced —
    /// charged by their recorded encodings, and each recorded payload is
    /// handed to the observation facility's bounded outbox through a
    /// nonblocking `try_send`, so no observer callback ever runs here. A
    /// refused handoff is visible loss, never a stall.
    pub fn promote_staged(&self) {
        let current = self.fence.epoch();
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        let mut records = std::mem::take(&mut table.staged);
        let candidate_loss = table.staged_loss;
        table.staged_bytes = 0;
        table.staged_loss = false;
        if candidate_loss {
            table.overflow = true;
        }
        // A record staged under an older epoch belongs to a generation
        // the reset already fenced: it is never published as accepted.
        records.retain(|staged| staged.epoch == current);
        if records.is_empty() {
            return;
        }
        // Capture the current dispatcher once: a plain clone that runs no
        // subscriber code. The observation facility replays the payloads
        // through it outside the acceptance path.
        let dispatch = dispatcher::get_default(|current| current.clone());
        let outbox = outbox();
        for staged in records {
            let StagedRecord {
                record,
                encoded,
                epoch,
            } = staged;
            if table.accepted.len() < MAX_DIARY_RECORDS
                && table.bytes + encoded.len() <= MAX_DIARY_BYTES
            {
                table.bytes += encoded.len();
                table.accepted.push_back(record);
            } else {
                table.overflow = true;
            }
            if let Err(TrySendError::Full(_) | TrySendError::Disconnected(_)) =
                outbox.try_send(OutboxJob {
                    dispatch: dispatch.clone(),
                    payload: encoded,
                    epoch,
                    fence: Arc::clone(&self.fence),
                })
            {
                // The observation facility is saturated or gone: visible
                // loss, never a stall.
                table.overflow = true;
            }
        }
    }

    /// Drops the staged collection of a failed candidate: a rejected,
    /// errored, or panicking invocation never publishes progress as
    /// accepted.
    pub fn discard_staged(&self) {
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        table.staged.clear();
        table.staged_bytes = 0;
        table.staged_loss = false;
    }

    /// Clears every staged and accepted record and advances the
    /// diagnostic epoch shared with the observation facility:
    /// reinitialization owns the diary, queued records of prior
    /// generations are dropped before any subscriber callback, and
    /// delivery loss of a prior generation cannot contaminate the fresh
    /// one — exactly like the adapter's fenced calls and captures.
    pub fn clear(&self) {
        // One fence-owned step: the epoch advance and loss clear
        // linearize together, before the table is touched (the two locks
        // are never held simultaneously).
        self.fence.advance();
        let mut table = self.table.lock().unwrap_or_else(|e| e.into_inner());
        table.staged.clear();
        table.staged_bytes = 0;
        table.staged_loss = false;
        table.accepted.clear();
        table.bytes = 0;
        table.overflow = false;
    }

    /// Copies the retained accepted records, oldest first. Bounded by the
    /// diary's item and byte caps.
    #[must_use]
    pub fn accepted(&self) -> Vec<BehaviorRecord> {
        self.table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .accepted
            .iter()
            .cloned()
            .collect()
    }

    /// Whether at least one record was refused — by the candidate
    /// collection's byte check, the accepted ring, the observation
    /// outbox, or a caught observer panic: bounded backpressure and
    /// delivery loss are one visible typed state, never a silent drop.
    #[must_use]
    pub fn overflowed(&self) -> bool {
        let local_loss = self
            .table
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .overflow;
        local_loss || self.fence.loss()
    }
}

/// The process-global observation outbox: one bounded queue and one
/// worker thread for the whole process, started on the first accepted
/// record. The worker runs until process exit and is never joined, so a
/// blocked observer callback can stall only this facility — later
/// records refuse at the outbox bound and surface as visible loss — and
/// never runtime acceptance or adapter teardown. Adapter replacement can
/// accumulate neither workers nor registrations: there is exactly one.
fn outbox() -> &'static SyncSender<OutboxJob> {
    OUTBOX.get_or_init(|| {
        let (sender, receiver) = sync_channel(MAX_OUTBOX_RECORDS);
        let _spawned = std::thread::Builder::new()
            .name("phoxal-behavior-observation".to_owned())
            .spawn(move || observation_loop(receiver));
        // If the thread could not start, the channel's receiver dies and
        // every handoff refuses with a visible loss — never a fault.
        sender
    })
}

/// Drains the observation outbox. Each accepted payload is replayed
/// through the dispatcher captured at acceptance, on this worker thread,
/// with any observer panic contained and reported as visible loss for
/// its own generation only.
fn observation_loop(receiver: Receiver<OutboxJob>) {
    while let Ok(job) = receiver.recv() {
        // Queued-record fencing: an old generation's record is dropped
        // before any subscriber callback runs.
        if job.epoch != job.fence.epoch() {
            continue;
        }
        let OutboxJob {
            dispatch,
            payload,
            epoch,
            fence,
        } = job;
        let delivered = catch_unwind(AssertUnwindSafe(|| {
            // Ordinary supported tracing emission under the captured
            // dispatcher: the macro's callsite registers through tracing
            // itself, and tracing reevaluates call-site interest whenever
            // dispatchers are created or dropped, so delivery follows the
            // live subscriber rather than any manual bypass.
            dispatcher::with_default(&dispatch, || {
                tracing::debug!(
                    target: "phoxal::boundary",
                    record = %payload,
                );
            });
        }));
        if delivered.is_err() {
            // A caught observer panic is visible delivery loss for this
            // generation only: the fence attributes it atomically with
            // reset, so an old job failing after reset cannot mark the
            // new generation.
            fence.mark_loss(epoch);
        }
    }
}

/// The serialized observer payload for one record. Serialized with a
/// real JSON serializer so arbitrary path text is escaped safely, and
/// carrying the explicit diagnostic epoch so an observer can always tell
/// an in-flight pre-reset record from new-generation progress.
#[derive(serde::Serialize)]
struct RecordJson<'a> {
    event: &'a str,
    epoch: u64,
    generation: u64,
    invocation: u64,
    time_ns: u64,
    status: String,
    kind: String,
    path: &'a str,
    pending_calls: usize,
    active_captures: usize,
}

/// A serializer destination that refuses to buffer beyond the diagnostic
/// byte budget, so producing (and rejecting) a representation can never
/// allocate past the bound.
struct BudgetWriter {
    out: String,
    budget: usize,
}

impl Write for BudgetWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.out.len() + buf.len() > self.budget {
            return Err(io::Error::new(
                io::ErrorKind::WriteZero,
                "diagnostic byte budget exceeded",
            ));
        }
        match std::str::from_utf8(buf) {
            Ok(text) => {
                self.out.reserve_exact(text.len());
                self.out.push_str(text);
                Ok(buf.len())
            }
            // serde_json emits valid UTF-8; anything else is treated as a
            // refusal rather than unsafe conversion.
            Err(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "serializer emitted invalid UTF-8",
            )),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// Produces one record's exact observer representation under the byte
/// budget, or `None` when the representation would exceed it. The only
/// allocation is the returned payload, which is bounded by the budget;
/// the serializer streams through the capped writer rather than building
/// an intermediate representation.
fn encode_bounded(record: &BehaviorRecord, epoch: u64) -> Option<String> {
    let mut writer = BudgetWriter {
        out: String::new(),
        budget: MAX_DIARY_BYTES,
    };
    serde_json::to_writer(
        &mut writer,
        &RecordJson {
            event: "behavior",
            epoch,
            generation: record.generation,
            invocation: record.invocation,
            time_ns: record.time_ns,
            status: format!("{:?}", record.status),
            kind: format!("{:?}", record.kind),
            path: &record.path,
            pending_calls: record.pending_calls,
            active_captures: record.active_captures,
        },
    )
    .ok()?;
    Some(writer.out)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The fence's reset/old-loss contract. The linearization point is
    /// the fence's single critical section: this test pins the exact
    /// interleaving that two independent atomics allowed — a job's
    /// failure for an old epoch arriving after a completed reset — which
    /// must not mark the fresh generation, while a failure for the
    /// current epoch must remain visible.
    #[test]
    fn an_old_failure_after_reset_cannot_mark_the_new_generation() {
        let fence = LossFence::default();
        let old_epoch = fence.epoch();
        // The late old-generation failure: with independent atomics this
        // ordering stored loss after the reset had already cleared it.
        fence.advance();
        fence.mark_loss(old_epoch);
        assert!(
            !fence.loss(),
            "an old generation's failure marked the fresh generation"
        );
        // A failure for the current epoch is still visible loss.
        fence.mark_loss(fence.epoch());
        assert!(fence.loss(), "a current-generation failure is visible");
        // And the next reset clears it atomically with the epoch.
        fence.advance();
        assert!(!fence.loss(), "reset clears loss with the epoch");
    }

    #[test]
    fn refusing_a_replacement_removes_the_staged_predecessor() {
        for path in [
            "x".repeat(MAX_DIARY_BYTES),
            "\n".repeat(MAX_DIARY_BYTES / 2),
        ] {
            let diary = BehaviorDiary::default();
            let mut record = BehaviorRecord {
                generation: 1,
                invocation: 0,
                time_ns: 0,
                status: TreeStatus::Running,
                kind: FailureKind::None,
                path: "sequence[0]".into(),
                pending_calls: 0,
                active_captures: 0,
            };
            diary.stage(record.clone());
            record.path = path;
            diary.stage(record);
            diary.promote_staged();
            assert!(diary.overflowed());
            assert!(
                diary.accepted().is_empty(),
                "refused replacement retained stale Running predecessor"
            );
        }
    }

    /// Rejecting an oversized public record never allocates its encoding:
    /// the raw path length is checked before any serialization work.
    #[test]
    fn rejecting_an_oversized_path_does_no_serialization_work() {
        let record = BehaviorRecord {
            generation: 1,
            invocation: 0,
            time_ns: 0,
            status: TreeStatus::Running,
            kind: FailureKind::None,
            path: "x".repeat(MAX_DIARY_BYTES + 1),
            pending_calls: 0,
            active_captures: 0,
        };
        // Path length alone proves the encoding cannot fit: the envelope
        // only adds bytes around it.
        assert!(record.path.len() >= MAX_DIARY_BYTES);
        let diary = BehaviorDiary::default();
        diary.stage(record);
        // The candidate's refusal surfaces when the candidate commits.
        diary.promote_staged();
        assert!(diary.overflowed(), "the refusal is visible loss");
        assert!(diary.accepted().is_empty());
    }

    /// A record just under the raw-path bound is still refused when its
    /// full encoding crosses the budget, and that refusal is visible.
    #[test]
    fn encoding_budget_refusals_are_visible() {
        let diary = BehaviorDiary::default();
        // A path near the bound plus the envelope must exceed it.
        let record = BehaviorRecord {
            generation: 1,
            invocation: 0,
            time_ns: 0,
            status: TreeStatus::Running,
            kind: FailureKind::None,
            path: "x".repeat(MAX_DIARY_BYTES - 1),
            pending_calls: 0,
            active_captures: 0,
        };
        diary.stage(record);
        diary.promote_staged();
        assert!(
            diary.overflowed(),
            "the budget-capped serializer refused the near-bound record visibly"
        );
        assert!(diary.accepted().is_empty());
    }
}
