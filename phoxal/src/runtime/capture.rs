//! Bounded typed captures of already-admitted input.
//!
//! A capture is a bounded tap of one queued input field, activated by a
//! behavior-tree node at an invocation boundary and drained by a later
//! wait. It copies already-admitted items before ordinary handlers run,
//! so a handler and an explicitly registered capture can both observe the
//! same event; call completions stay exclusive. There is no second
//! transport subscription and no callback from transport threads.

use std::collections::{BTreeMap, VecDeque};
use std::marker::PhantomData;
use std::sync::Mutex;

/// Upper bound of simultaneously active captures per runtime.
pub const MAX_ACTIVE_CAPTURES: usize = 8;
/// Default item bound of one capture, from the authored input policy.
pub const MAX_CAPTURED_ITEMS: usize = 16;
/// Default byte bound of one capture's retained payloads.
pub const MAX_CAPTURED_BYTES: usize = 8 * 1024;

/// A typed input-field descriptor, generated per queued input endpoint.
pub struct InputDescriptor<T> {
    /// The input field's declared name.
    pub field: &'static str,
    marker: PhantomData<fn() -> T>,
}

impl<T> InputDescriptor<T> {
    /// Constructs one descriptor; generation owns this, applications
    /// consume the generated constants.
    #[must_use]
    pub const fn new(field: &'static str) -> Self {
        Self {
            field,
            marker: PhantomData,
        }
    }
}

/// One typed capture identity handed from `observe` to `wait_event`.
pub struct CaptureHandle<T> {
    /// The registry-local capture identity.
    pub id: u64,
    /// The captured input field's declared name.
    pub field: &'static str,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for CaptureHandle<T> {
    fn clone(&self) -> Self {
        Self {
            id: self.id,
            field: self.field,
            marker: PhantomData,
        }
    }
}

impl<T> CaptureHandle<T> {
    pub(super) fn new(id: u64, field: &'static str) -> Self {
        Self {
            id,
            field,
            marker: PhantomData,
        }
    }
}

/// The bounded result of polling one capture.
pub enum CapturePoll<T> {
    /// No eligible event has arrived yet.
    Pending,
    /// One retained event, removed from the capture.
    Event(T),
    /// The capture's bounds overflowed: events were dropped after the
    /// bound, a visible typed failure rather than a silent miss. Loss is
    /// authoritative and checked before any retained item is examined.
    Overflow,
    /// The capture was released or never activated.
    Gone,
    /// A retained item failed its typed decode: an integrity failure of
    /// this execution, distinct from bounded loss.
    Corrupt(String),
}

/// One active capture slot.
#[derive(Debug)]
struct Slot {
    field: &'static str,
    /// The invocation index at activation: items admitted at or before it
    /// are excluded from the capture.
    activated_at: u64,
    items: VecDeque<Vec<u8>>,
    bytes: usize,
    overflow: bool,
}

/// The runtime-owned registry of active captures.
#[derive(Default, Debug)]
pub struct CaptureRegistry {
    slots: Mutex<BTreeMap<u64, Slot>>,
}

/// Allocates one fresh capture identity. Ids are process-unique so a
/// handle minted at construction time never collides with another
/// adapter's slots; the slot itself is created at activation, so an
/// unused handle retains no resources.
pub fn allocate_handle<T>(field: &'static str) -> CaptureHandle<T> {
    CaptureHandle::new(next_capture_id(), field)
}

fn next_capture_id() -> u64 {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::Relaxed)
}

impl CaptureRegistry {
    /// Activates one capture at the given invocation: later admissions
    /// copy into it until it is released.
    pub fn activate(&self, id: u64, field: &'static str, activated_at: u64) -> bool {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if slots.len() >= MAX_ACTIVE_CAPTURES && !slots.contains_key(&id) {
            return false;
        }
        slots.insert(
            id,
            Slot {
                field,
                activated_at,
                items: VecDeque::new(),
                bytes: 0,
                overflow: false,
            },
        );
        true
    }

    /// Releases one capture: later items are fenced from it permanently.
    pub fn release(&self, id: u64) {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
    }

    /// The number of currently active capture slots.
    #[must_use]
    pub fn active_count(&self) -> usize {
        self.slots.lock().unwrap_or_else(|e| e.into_inner()).len()
    }

    /// Whether any active capture watches the named field.
    pub fn has_active(&self, field: &str) -> bool {
        self.slots
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .any(|slot| slot.field == field)
    }

    /// Copies one admitted item into every active capture of its field,
    /// when the item was admitted after the capture's activation.
    /// Called by generated dispatch before ordinary handlers run.
    pub fn copy_admitted(&self, field: &str, invocation: u64, payload: &[u8]) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        for slot in slots.values_mut() {
            if slot.field != field || invocation <= slot.activated_at {
                continue;
            }
            if slot.items.len() >= MAX_CAPTURED_ITEMS
                || slot.bytes + payload.len() > MAX_CAPTURED_BYTES
            {
                // Overflow is a visible typed failure, never a silent
                // drop that could look like a successful wait.
                slot.overflow = true;
                continue;
            }
            slot.bytes += payload.len();
            slot.items.push_back(payload.to_vec());
        }
    }

    /// Polls one capture for its next retained event, decoding through
    /// the supplied decoder.
    ///
    /// The sticky loss state is authoritative: once items were dropped at
    /// the bound, a matching retained item can never turn the wait into
    /// success, so overflow is checked before any item is examined.
    /// Decode failure stays a distinct integrity outcome.
    pub fn poll<T>(&self, id: u64, decode: impl Fn(&[u8]) -> crate::Result<T>) -> CapturePoll<T> {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        let Some(slot) = slots.get_mut(&id) else {
            return CapturePoll::Gone;
        };
        if slot.overflow {
            return CapturePoll::Overflow;
        }
        if let Some(payload) = slot.items.pop_front() {
            slot.bytes = slot.bytes.saturating_sub(payload.len());
            return match decode(&payload) {
                Ok(event) => CapturePoll::Event(event),
                Err(error) => CapturePoll::Corrupt(error.to_string()),
            };
        }
        CapturePoll::Pending
    }

    /// Releases every capture slot: reinitialization owns the registry and
    /// fences every prior handle — their identities are never reused, so
    /// an old handle can neither consume capacity nor receive new-era
    /// input.
    pub fn clear(&self) {
        self.slots.lock().unwrap_or_else(|e| e.into_inner()).clear();
    }
}
