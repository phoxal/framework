use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::fmt::Write as _;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::api;
use crate::participant::lock;
use phoxal_bus::{BusHandle, DiagnosticPublisher};
use tokio::sync::mpsc;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

const DEFAULT_BUFFER_CAPACITY: usize = 1024;
const MAX_RECORD_TEXT_BYTES: usize = 64 * 1024;
const MAX_TARGET_BYTES: usize = 256;
const MAX_MESSAGE_BYTES: usize = 16 * 1024;
const MAX_FIELD_NAME_BYTES: usize = 128;
const MAX_FIELD_VALUE_BYTES: usize = 8 * 1024;
const MAX_FIELDS: usize = 64;

thread_local! {
    static IN_BUS_LOG_PUBLISH: Cell<bool> = const { Cell::new(false) };
}

#[derive(Debug)]
struct ActivePublisher {
    token: u64,
    sender: mpsc::Sender<LogRecord>,
}

#[derive(Debug)]
pub(crate) struct BusLogState {
    active: Mutex<Option<ActivePublisher>>,
    dropped: AtomicU64,
    max_level: AtomicU8,
    next_token: AtomicU64,
}

impl BusLogState {
    fn new() -> Self {
        Self {
            active: Mutex::new(None),
            dropped: AtomicU64::new(0),
            max_level: AtomicU8::new(Self::gate(Level::INFO)),
            next_token: AtomicU64::new(1),
        }
    }

    /// A level as this gate ranks it: smaller is more severe, so "at most
    /// `max_level`" is a single integer comparison on the hot path rather than
    /// a `tracing::Level` ordering behind a lock.
    const fn gate(level: Level) -> u8 {
        match level {
            Level::ERROR => 1,
            Level::WARN => 2,
            Level::INFO => 3,
            Level::DEBUG => 4,
            Level::TRACE => 5,
        }
    }

    fn try_enqueue(&self, record: LogRecord) {
        let active = lock(&self.active);
        let Some(active) = active.as_ref() else {
            return;
        };
        match active.sender.try_send(record) {
            Ok(()) => {}
            Err(mpsc::error::TrySendError::Full(_)) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
            Err(mpsc::error::TrySendError::Closed(_)) => {}
        }
    }

    fn should_publish(&self, metadata: &Metadata<'_>) -> bool {
        self.allows(*metadata.level(), metadata.target())
    }

    fn allows(&self, level: Level, target: &str) -> bool {
        if target_is_filtered(target) {
            return false;
        }
        if IN_BUS_LOG_PUBLISH.with(Cell::get) {
            return false;
        }
        Self::gate(level) <= self.max_level.load(Ordering::Relaxed)
    }

    fn install_sender(&self, sender: mpsc::Sender<LogRecord>) -> u64 {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let mut active = lock(&self.active);
        *active = Some(ActivePublisher { token, sender });
        token
    }

    fn clear_sender(&self, token: u64) {
        let mut active = lock(&self.active);
        if active.as_ref().is_some_and(|active| active.token == token) {
            *active = None;
        }
    }

    fn take_dropped(&self) -> u32 {
        let taken = self
            .dropped
            .load(Ordering::Relaxed)
            .min(u64::from(u32::MAX));
        self.dropped.fetch_sub(taken, Ordering::Relaxed);
        taken as u32
    }
}

pub(crate) struct BusLogLayer {
    state: Arc<BusLogState>,
}

impl BusLogLayer {
    pub(crate) fn new(state: Arc<BusLogState>) -> Self {
        Self { state }
    }
}

impl<S> Layer<S> for BusLogLayer
where
    S: tracing::Subscriber + for<'span> LookupSpan<'span>,
{
    fn on_event(&self, event: &Event<'_>, _ctx: Context<'_, S>) {
        if !self.state.should_publish(event.metadata()) {
            return;
        }
        self.state.try_enqueue(LogRecord::from_event(event));
    }
}

#[derive(Clone, Debug)]
struct LogRecord {
    time: api::logs::Timestamp,
    level: api::logs::Level,
    target: String,
    message: String,
    fields: BTreeMap<String, api::logs::LogValue>,
    truncated: u32,
}

impl LogRecord {
    /// A tracing level as the wire enum names it.
    fn wire_level(level: Level) -> api::logs::Level {
        match level {
            Level::ERROR => api::logs::Level::Error,
            Level::WARN => api::logs::Level::Warn,
            Level::INFO => api::logs::Level::Info,
            Level::DEBUG => api::logs::Level::Debug,
            Level::TRACE => api::logs::Level::Trace,
        }
    }

    fn from_event(event: &Event<'_>) -> Self {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        let (target, target_truncated) = bounded_text(event.metadata().target(), MAX_TARGET_BYTES);
        Self {
            time: timestamp_now(),
            level: Self::wire_level(*event.metadata().level()),
            target,
            message: visitor.message.unwrap_or_default(),
            fields: visitor.fields,
            truncated: visitor
                .truncations
                .saturating_add(u32::from(target_truncated)),
        }
    }

    fn into_event(self, seq: u64, dropped: u32) -> api::logs::Event {
        api::logs::Event {
            seq,
            time: self.time,
            level: self.level,
            target: self.target,
            message: self.message,
            fields: self.fields,
            dropped,
            truncated: self.truncated,
        }
    }
}

struct FieldVisitor {
    message: Option<String>,
    fields: BTreeMap<String, api::logs::LogValue>,
    remaining_text_bytes: usize,
    truncations: u32,
}

impl Default for FieldVisitor {
    fn default() -> Self {
        Self {
            message: None,
            fields: BTreeMap::new(),
            remaining_text_bytes: MAX_RECORD_TEXT_BYTES,
            truncations: 0,
        }
    }
}

impl FieldVisitor {
    fn bounded(&mut self, value: &str, per_value_limit: usize) -> String {
        let limit = per_value_limit.min(self.remaining_text_bytes);
        let (bounded, truncated) = bounded_text(value, limit);
        self.remaining_text_bytes = self.remaining_text_bytes.saturating_sub(bounded.len());
        self.truncations = self.truncations.saturating_add(u32::from(truncated));
        bounded
    }

    fn record_value(&mut self, field: &Field, mut value: api::logs::LogValue) {
        let name = field.name();
        if name == "message" {
            let message = log_value_to_string(&value);
            self.message = Some(self.bounded(&message, MAX_MESSAGE_BYTES));
        } else {
            if self.fields.len() >= MAX_FIELDS && !self.fields.contains_key(name) {
                self.truncations = self.truncations.saturating_add(1);
                return;
            }
            let name = self.bounded(name, MAX_FIELD_NAME_BYTES);
            if let api::logs::LogValue::String(text) = &mut value {
                *text = self.bounded(text, MAX_FIELD_VALUE_BYTES);
            }
            self.fields.insert(name, value);
        }
    }
}

impl Visit for FieldVisitor {
    fn record_bool(&mut self, field: &Field, value: bool) {
        self.record_value(field, api::logs::LogValue::Bool(value));
    }

    fn record_i64(&mut self, field: &Field, value: i64) {
        self.record_value(field, api::logs::LogValue::I64(value));
    }

    fn record_u64(&mut self, field: &Field, value: u64) {
        self.record_value(field, api::logs::LogValue::U64(value));
    }

    fn record_f64(&mut self, field: &Field, value: f64) {
        self.record_value(field, api::logs::LogValue::F64(value));
    }

    fn record_str(&mut self, field: &Field, value: &str) {
        self.record_value(field, api::logs::LogValue::String(value.to_string()));
    }

    fn record_debug(&mut self, field: &Field, value: &dyn fmt::Debug) {
        let per_value_limit = if field.name() == "message" {
            MAX_MESSAGE_BYTES
        } else {
            MAX_FIELD_VALUE_BYTES
        };
        let mut formatted = BoundedFormatter::new(per_value_limit.min(self.remaining_text_bytes));
        let _ = write!(&mut formatted, "{value:?}");
        if formatted.truncated {
            self.truncations = self.truncations.saturating_add(1);
        }
        self.record_value(field, api::logs::LogValue::String(formatted.value));
    }
}

struct BoundedFormatter {
    value: String,
    limit: usize,
    truncated: bool,
}

impl BoundedFormatter {
    fn new(limit: usize) -> Self {
        Self {
            value: String::with_capacity(limit),
            limit,
            truncated: false,
        }
    }
}

impl fmt::Write for BoundedFormatter {
    fn write_str(&mut self, value: &str) -> fmt::Result {
        let remaining = self.limit.saturating_sub(self.value.len());
        let (bounded, truncated) = bounded_text(value, remaining);
        self.value.push_str(&bounded);
        self.truncated |= truncated;
        Ok(())
    }
}

fn bounded_text(value: &str, max_bytes: usize) -> (String, bool) {
    if value.len() <= max_bytes {
        return (value.to_string(), false);
    }
    let mut end = max_bytes;
    while !value.is_char_boundary(end) {
        end -= 1;
    }
    (value[..end].to_string(), true)
}

pub(crate) fn new_state() -> Arc<BusLogState> {
    Arc::new(BusLogState::new())
}

pub(crate) fn attach(bus: BusHandle, participant_id: &str) -> (BusLogGuard, BusLogTask) {
    attach_with_capacity(bus, participant_id, DEFAULT_BUFFER_CAPACITY)
}

fn attach_with_capacity(
    bus: BusHandle,
    participant_id: &str,
    capacity: usize,
) -> (BusLogGuard, BusLogTask) {
    let state = crate::participant::runner::bus_log_state();
    let (sender, receiver) = mpsc::channel(capacity.max(1));
    let token = state.install_sender(sender);
    let participant_id = participant_id.to_string();
    (
        BusLogGuard {
            token,
            state: Arc::clone(&state),
        },
        BusLogTask {
            state,
            bus,
            participant_id,
            receiver,
        },
    )
}

pub(crate) struct BusLogGuard {
    token: u64,
    state: Arc<BusLogState>,
}

impl BusLogGuard {
    pub(crate) fn shutdown(self) {
        self.state.clear_sender(self.token);
    }
}

impl Drop for BusLogGuard {
    fn drop(&mut self) {
        self.state.clear_sender(self.token);
    }
}

/// The drain operation is returned separately from [`BusLogGuard`] so the
/// runner can register it in its one managed task group before participant
/// setup. The guard only owns the diagnostic sender lease; dropping it stops
/// new records from entering the queue.
pub(crate) struct BusLogTask {
    state: Arc<BusLogState>,
    bus: BusHandle,
    participant_id: String,
    receiver: mpsc::Receiver<LogRecord>,
}

impl BusLogTask {
    pub(crate) async fn run(self) -> crate::Result<()> {
        drain_loop(self.state, self.bus, self.participant_id, self.receiver).await
    }
}

async fn drain_loop(
    state: Arc<BusLogState>,
    bus: BusHandle,
    participant_id: String,
    mut receiver: mpsc::Receiver<LogRecord>,
) -> crate::Result<()> {
    let topic = api::topic::owner().logs(&participant_id).topic();
    let publisher = DiagnosticPublisher::<api::logs::Event>::new(bus, &topic)?;
    let mut seq = 0_u64;
    while let Some(record) = receiver.recv().await {
        let dropped = state.take_dropped();
        let event = record.into_event(seq, dropped);
        seq = seq.wrapping_add(1);
        let result = IN_BUS_LOG_PUBLISH.with(|guard| {
            guard.set(true);
            let result = publisher.publish(event);
            guard.set(false);
            result
        });
        if result.is_err() {
            state
                .dropped
                .fetch_add(u64::from(dropped).saturating_add(1), Ordering::Relaxed);
        }
    }
    Ok(())
}

fn target_is_filtered(target: &str) -> bool {
    target.starts_with("zenoh")
        || target.starts_with("phoxal_bus")
        || target.starts_with("phoxal.bus")
}

fn timestamp_now() -> api::logs::Timestamp {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => api::logs::Timestamp {
            unix_seconds: i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
            nanos: duration.subsec_nanos(),
        },
        Err(error) => {
            let duration = error.duration();
            api::logs::Timestamp {
                unix_seconds: -i64::try_from(duration.as_secs()).unwrap_or(i64::MAX),
                nanos: duration.subsec_nanos(),
            }
        }
    }
}

fn log_value_to_string(value: &api::logs::LogValue) -> String {
    match value {
        api::logs::LogValue::Bool(value) => value.to_string(),
        api::logs::LogValue::I64(value) => value.to_string(),
        api::logs::LogValue::U64(value) => value.to_string(),
        api::logs::LogValue::F64(value) => value.to_string(),
        api::logs::LogValue::String(value) => value.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record() -> LogRecord {
        LogRecord {
            time: api::logs::Timestamp {
                unix_seconds: 1,
                nanos: 2,
            },
            level: api::logs::Level::Info,
            target: "test".to_string(),
            message: "hello".to_string(),
            fields: BTreeMap::new(),
            truncated: 0,
        }
    }

    #[test]
    fn level_gate_defaults_to_info_plus() {
        let state = BusLogState::new();
        assert!(state.allows(Level::INFO, "app"));
        assert!(state.allows(Level::WARN, "app"));
        assert!(!state.allows(Level::DEBUG, "app"));
    }

    #[test]
    fn reentrancy_targets_are_filtered() {
        let state = BusLogState::new();
        assert!(!state.allows(Level::INFO, "zenoh"));
        assert!(!state.allows(Level::INFO, "zenoh_transport"));
        assert!(!state.allows(Level::INFO, "phoxal_bus::session"));
        assert!(!state.allows(Level::INFO, "phoxal.bus"));
    }

    #[tokio::test]
    async fn bounded_sender_counts_drops_under_flood() {
        let state = BusLogState::new();
        let (sender, mut receiver) = mpsc::channel(1);
        let token = state.install_sender(sender);

        state.try_enqueue(record());
        state.try_enqueue(record());
        state.try_enqueue(record());

        assert!(receiver.try_recv().is_ok());
        assert_eq!(state.take_dropped(), 2);
        state.clear_sender(token);
    }

    #[tokio::test]
    async fn structured_bus_layer_captures_without_a_fmt_layer() {
        use tracing_subscriber::layer::SubscriberExt;

        let state = Arc::new(BusLogState::new());
        let (sender, mut receiver) = mpsc::channel(1);
        let token = state.install_sender(sender);
        let subscriber = tracing_subscriber::registry().with(BusLogLayer::new(Arc::clone(&state)));

        tracing::subscriber::with_default(subscriber, || {
            tracing::info!(target: "runtime", marker = 7_u64, "ready");
        });

        let captured = receiver.try_recv().expect("structured event captured");
        assert_eq!(captured.target, "runtime");
        assert_eq!(captured.message, "ready");
        assert!(matches!(
            captured.fields.get("marker"),
            Some(api::logs::LogValue::U64(7))
        ));
        state.clear_sender(token);
    }

    /// The decision trace is evidence an operator can actually read, not
    /// in-process state. Every lease transition has to survive the real bus-log
    /// pipeline - its target must not be one of the filtered ones - and carry
    /// the producer, the sender's sequence, this receiver's observation ordinal,
    /// and the decision.
    #[tokio::test]
    async fn lease_decisions_reach_the_bus_log_with_their_full_provenance() {
        use crate::bus::{Lease, LocalInstant, ProducerId, RobotInstant, TimelineId};
        use std::time::Duration;
        use tracing_subscriber::layer::SubscriberExt;

        assert!(
            !target_is_filtered(phoxal_bus::LEASE_TRACE_TARGET),
            "the decision trace must not be dropped before it reaches the bus"
        );

        let state = Arc::new(BusLogState::new());
        let (sender, mut receiver) = mpsc::channel(16);
        let token = state.install_sender(sender);
        let subscriber = tracing_subscriber::registry().with(BusLogLayer::new(Arc::clone(&state)));

        let first =
            ProducerId::try_from((1_u128 << 124) | 1).expect("a test producer is canonical");
        let second =
            ProducerId::try_from((1_u128 << 124) | 2).expect("a test producer is canonical");
        let silence = Duration::from_millis(150);
        let start = LocalInstant::from_boot_ns(0);
        let step = RobotInstant::new(TimelineId::mint(), 0);

        tracing::subscriber::with_default(subscriber, || {
            let mut lease = Lease::new("motion/manual", silence, Duration::from_millis(500));
            lease.offer(first, 1, start, "go");
            lease.live(start, step);
            lease.offer(second, 0, start, "replacement");
            lease.offer(first, 9, start, "zombie");
            lease.live(start.saturating_add(silence), step);
        });

        let mut decisions = Vec::new();
        while let Ok(record) = receiver.try_recv() {
            assert_eq!(record.target, phoxal_bus::LEASE_TRACE_TARGET);
            let Some(api::logs::LogValue::String(decision)) = record.fields.get("decision") else {
                panic!("every lease record names its decision: {record:?}");
            };
            decisions.push((decision.clone(), record));
        }

        let names: Vec<&str> = decisions
            .iter()
            .map(|(decision, _)| decision.as_str())
            .collect();
        assert_eq!(
            names,
            vec!["producer_replaced", "rejected", "expired_silence"],
            "every transition is reported; ordinary renewals are the DEBUG half \
             of the same trace"
        );

        let (_, expired) = &decisions[2];
        assert_eq!(
            expired.fields.get("producer"),
            Some(&api::logs::LogValue::String(second.to_string())),
            "an expiry names the command that died, not just the lease"
        );
        assert_eq!(
            expired.fields.get("sequence"),
            Some(&api::logs::LogValue::U64(0))
        );
        assert_eq!(
            expired.fields.get("observation"),
            Some(&api::logs::LogValue::U64(2))
        );

        let (_, replaced) = &decisions[0];
        assert_eq!(
            replaced.fields.get("producer"),
            Some(&api::logs::LogValue::String(second.to_string()))
        );
        assert_eq!(
            replaced.fields.get("superseded"),
            Some(&api::logs::LogValue::String(first.to_string()))
        );
        assert_eq!(
            replaced.fields.get("sequence"),
            Some(&api::logs::LogValue::U64(0)),
            "a restarted producer restarting at zero is not a replay"
        );
        assert_eq!(
            replaced.fields.get("observation"),
            Some(&api::logs::LogValue::U64(2)),
            "the ordinate is the receiver's own arrival order"
        );
        assert_eq!(
            replaced.fields.get("input"),
            Some(&api::logs::LogValue::String("motion/manual".to_string()))
        );

        state.clear_sender(token);
    }

    #[test]
    fn no_bus_attached_is_a_noop() {
        let state = BusLogState::new();
        state.try_enqueue(record());
        assert_eq!(state.take_dropped(), 0);
    }

    #[test]
    fn dropped_counter_carries_values_above_the_wire_field_limit() {
        let state = BusLogState::new();
        state
            .dropped
            .store(u64::from(u32::MAX) + 7, Ordering::Relaxed);
        assert_eq!(state.take_dropped(), u32::MAX);
        assert_eq!(state.take_dropped(), 7);
    }

    #[test]
    fn record_truncation_is_distinct_from_lost_record_count() {
        let mut record = record();
        record.truncated = 4;
        let event = record.into_event(7, 2);
        assert_eq!(event.dropped, 2);
        assert_eq!(event.truncated, 4);
    }

    #[test]
    fn debug_formatting_and_text_helpers_are_byte_bounded() {
        struct HugeDebug;
        impl fmt::Debug for HugeDebug {
            fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
                for _ in 0..100_000 {
                    formatter.write_str("é")?;
                }
                Ok(())
            }
        }

        let mut formatted = BoundedFormatter::new(101);
        write!(&mut formatted, "{:?}", HugeDebug).expect("format debug value");
        assert!(formatted.truncated);
        assert!(formatted.value.len() <= 101);
        assert!(formatted.value.is_char_boundary(formatted.value.len()));

        let (bounded, truncated) = bounded_text(&"é".repeat(100), 17);
        assert!(truncated);
        assert!(bounded.len() <= 17);
    }
}
