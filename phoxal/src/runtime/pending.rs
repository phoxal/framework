//! The runtime-owned pending-call table.
//!
//! One bounded ledger of every staged generated call's exclusive completion
//! owner — a direct `#[complete]` handler field or the concrete tree
//! generation whose leaf submitted it. Records are staged with the
//! candidate, promoted when the owning boundary accepts it, and discarded
//! when preparation fails or the runtime is reinitialized; retirement
//! releases a call's local eligibility whether or not a reply is already
//! in the current cut.
//!
//! Ticket ids qualify the execution epoch that every runtime
//! initialization draws, so a result minted by one execution can never be
//! claimed by another. Absence of a record is the only invalidation:
//! unowned results are refused and dropped by delivery, with no
//! unbounded tombstone history.
//!
//! The ledger is a mutex rather than a thread-local or `RefCell` because
//! the runtime adapter is a shared, `Sync` value for launch glue and the
//! harness; every access covers only map operations, never authored code,
//! so a poisoned lock is recovered rather than propagated.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Mutex, MutexGuard, PoisonError};

/// Upper bound of tracked pending calls, direct and tree together; staging
/// beyond it is a visible error rather than unbounded growth.
pub const MAX_PENDING_CALLS: usize = 256;

/// Allocates the next globally distinct execution epoch.
///
/// Every runtime initialization draws a fresh epoch — a process-wide
/// unique identity that does not depend on the host thread — so ticket
/// ids minted by one execution can never alias the ids of another, even
/// when both start from invocation zero on different threads. The adapter
/// captures its epoch at initialization; later invocations read the
/// captured value, so host-thread migration never changes an execution's
/// tickets.
pub fn next_execution_epoch() -> u64 {
    static EPOCH: AtomicU64 = AtomicU64::new(0);
    EPOCH.fetch_add(1, Ordering::Relaxed)
}

/// The exclusive owner of one pending call's completion.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum PendingOwner {
    /// The direct `#[complete]` handler of the named call field.
    Direct(&'static str),
    /// The behavior-tree generation whose leaf submitted the call; only
    /// that generation's leaves poll and retire it.
    Tree {
        /// The construction generation of the owning tree.
        generation: u64,
    },
}

/// One runtime's bounded pending-call ownership ledger.
///
/// Owned by the generated runtime adapter and reached through the context
/// view; authored code never constructs or names it.
#[derive(Debug, Default)]
pub struct PendingCalls {
    records: Mutex<Table>,
}

#[derive(Debug, Default)]
struct Table {
    staged: BTreeMap<u128, PendingOwner>,
    committed: BTreeMap<u128, PendingOwner>,
}

impl PendingCalls {
    /// Locks the ledger; poisoning is impossible across this crate's own
    /// map operations, so the table is recovered instead of faulting.
    fn lock(&self) -> MutexGuard<'_, Table> {
        self.records.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records one staged call's owner. Recording happens while the
    /// candidate is being prepared; the record is promoted when the owning
    /// boundary accepts the candidate, and discarded when preparation
    /// fails or the runtime is reinitialized.
    pub fn stage(&self, ticket: u128, owner: PendingOwner) -> crate::Result<()> {
        let mut table = self.lock();
        if table.staged.len() + table.committed.len() >= MAX_PENDING_CALLS
            && !table.staged.contains_key(&ticket)
            && !table.committed.contains_key(&ticket)
        {
            return Err(crate::anyhow!(
                "pending generated calls exceed {MAX_PENDING_CALLS}"
            ));
        }
        if let Some(live) = table.committed.get(&ticket)
            && *live != owner
        {
            return Err(crate::anyhow!(
                "pending call ownership collision: ticket already owned by a \
                 different live owner"
            ));
        }
        table.staged.insert(ticket, owner);
        crate::Result::Ok(())
    }

    /// Promotes every staged record to committed: the owning boundary
    /// accepted the complete candidate that staged them.
    pub fn promote_staged(&self) {
        let mut table = self.lock();
        let staged = std::mem::take(&mut table.staged);
        for (ticket, owner) in staged {
            table.committed.insert(ticket, owner);
        }
    }

    /// Discards every staged record: the candidate that staged them failed
    /// preparation, so none of its calls may ever claim a result.
    pub fn discard_staged(&self) {
        self.lock().staged.clear();
    }

    /// Claims one ticket for its direct field handler: returns and
    /// consumes the record when the ticket is a committed direct-owned
    /// call, so the completion is delivered exactly once to exactly the
    /// field that staged it.
    pub fn claim_direct(&self, ticket: u128) -> Option<&'static str> {
        let mut table = self.lock();
        match table.committed.remove(&ticket) {
            Some(PendingOwner::Direct(field)) => Some(field),
            other => {
                if let Some(owner) = other {
                    table.committed.insert(ticket, owner);
                }
                None
            }
        }
    }

    /// Reports whether the ticket is a committed call of the given tree
    /// generation, without consuming anything: a leaf may poll cuts that
    /// do not yet hold its reply, and polling must never spend the call's
    /// ownership.
    #[must_use]
    pub fn is_tree_call(&self, ticket: u128, generation: u64) -> bool {
        self.lock().committed.get(&ticket) == Some(&PendingOwner::Tree { generation })
    }

    /// Reports whether the ticket is committed to any tree owner.
    #[must_use]
    pub fn is_any_tree_call(&self, ticket: u128) -> bool {
        matches!(
            self.lock().committed.get(&ticket),
            Some(PendingOwner::Tree { .. })
        )
    }

    /// Consumes one committed tree-owned ticket, releasing its ledger
    /// slot; called only after its result was actually taken from the
    /// retained store, so the completion is delivered exactly once.
    pub fn consume_tree(&self, ticket: u128) {
        let mut table = self.lock();
        table.committed.remove(&ticket);
        table.staged.remove(&ticket);
    }

    /// Reports one ticket's committed owner, if it is still pending.
    #[must_use]
    pub fn owner_of(&self, ticket: u128) -> Option<PendingOwner> {
        self.lock().committed.get(&ticket).copied()
    }

    /// Retires one call's local completion eligibility: the record is
    /// removed whether it is staged or committed, and because ticket ids
    /// carry their execution's epoch, no later record can ever reuse the
    /// id — absence alone invalidates the retired call's results.
    pub fn retire(&self, ticket: u128) {
        let mut table = self.lock();
        table.staged.remove(&ticket);
        table.committed.remove(&ticket);
    }

    /// Discards every record — reinitialization and reset release all
    /// bounded storage and fence every prior ticket of this adapter: its
    /// epoch is never drawn again.
    pub fn clear(&self) {
        let mut table = self.lock();
        table.staged.clear();
        table.committed.clear();
    }

    /// The number of tracked pending calls, staged and committed.
    #[must_use]
    pub fn len(&self) -> usize {
        let table = self.lock();
        table.staged.len() + table.committed.len()
    }

    /// Reports whether no call is pending.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}
