use std::cell::Cell;
use std::collections::BTreeMap;
use std::fmt;
use std::sync::atomic::{AtomicU8, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{SystemTime, UNIX_EPOCH};

use phoxal_api::v1 as api;
use phoxal_bus::{Bus, LogicalTime, OwnerCap, Publisher};
use tokio::sync::mpsc;
use tokio::task::JoinHandle;
use tracing::field::{Field, Visit};
use tracing::{Event, Level, Metadata};
use tracing_subscriber::Layer;
use tracing_subscriber::layer::Context;
use tracing_subscriber::registry::LookupSpan;

const DEFAULT_BUFFER_CAPACITY: usize = 1024;

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
            max_level: AtomicU8::new(level_to_gate(Level::INFO)),
            next_token: AtomicU64::new(1),
        }
    }

    fn set_max_level(&self, level: Level) {
        self.max_level
            .store(level_to_gate(level), Ordering::Relaxed);
    }

    fn try_enqueue(&self, record: LogRecord) {
        let active = self.active.lock().expect("bus log mutex poisoned");
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
        level_to_gate(level) <= self.max_level.load(Ordering::Relaxed)
    }

    fn install_sender(&self, sender: mpsc::Sender<LogRecord>) -> u64 {
        let token = self.next_token.fetch_add(1, Ordering::Relaxed);
        let mut active = self.active.lock().expect("bus log mutex poisoned");
        *active = Some(ActivePublisher { token, sender });
        token
    }

    fn clear_sender(&self, token: u64) {
        let mut active = self.active.lock().expect("bus log mutex poisoned");
        if active.as_ref().is_some_and(|active| active.token == token) {
            *active = None;
        }
    }

    fn take_dropped(&self) -> u32 {
        self.dropped
            .swap(0, Ordering::Relaxed)
            .min(u64::from(u32::MAX)) as u32
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
}

impl LogRecord {
    fn from_event(event: &Event<'_>) -> Self {
        let mut visitor = FieldVisitor::default();
        event.record(&mut visitor);
        Self {
            time: timestamp_now(),
            level: level_from_tracing(*event.metadata().level()),
            target: event.metadata().target().to_string(),
            message: visitor.message.unwrap_or_default(),
            fields: visitor.fields,
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
        }
    }

    fn logical_time(&self) -> LogicalTime {
        let seconds = u64::try_from(self.time.unix_seconds).unwrap_or(0);
        let nanos = seconds
            .saturating_mul(1_000_000_000)
            .saturating_add(u64::from(self.time.nanos));
        LogicalTime::new(0, nanos)
    }
}

#[derive(Default)]
struct FieldVisitor {
    message: Option<String>,
    fields: BTreeMap<String, api::logs::LogValue>,
}

impl FieldVisitor {
    fn record_value(&mut self, field: &Field, value: api::logs::LogValue) {
        let name = field.name();
        if name == "message" {
            self.message = Some(log_value_to_string(&value));
        } else {
            self.fields.insert(name.to_string(), value);
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
        self.record_value(field, api::logs::LogValue::String(format!("{value:?}")));
    }
}

pub(crate) fn new_state_from_env() -> Arc<BusLogState> {
    let state = Arc::new(BusLogState::new());
    if let Some(level) = std::env::var("PHOXAL_BUS_LOG_LEVEL")
        .ok()
        .and_then(|value| parse_level(&value))
    {
        state.set_max_level(level);
    }
    state
}

pub(crate) fn attach(bus: Bus, participant_id: &str) -> BusLogGuard {
    attach_with_capacity(bus, participant_id, DEFAULT_BUFFER_CAPACITY)
}

fn attach_with_capacity(bus: Bus, participant_id: &str, capacity: usize) -> BusLogGuard {
    let state = crate::participant::runner::bus_log_state();
    let (sender, receiver) = mpsc::channel(capacity.max(1));
    let token = state.install_sender(sender);
    let participant_id = participant_id.to_string();
    let task = tokio::spawn(drain_loop(
        Arc::clone(&state),
        bus,
        participant_id,
        receiver,
    ));
    BusLogGuard {
        token,
        state,
        task: Some(task),
    }
}

pub(crate) struct BusLogGuard {
    token: u64,
    state: Arc<BusLogState>,
    task: Option<JoinHandle<()>>,
}

impl BusLogGuard {
    pub(crate) async fn shutdown(mut self) {
        self.state.clear_sender(self.token);
        if let Some(task) = self.task.take() {
            let _ = task.await;
        }
    }
}

impl Drop for BusLogGuard {
    fn drop(&mut self) {
        self.state.clear_sender(self.token);
        if let Some(task) = self.task.take() {
            task.abort();
        }
    }
}

async fn drain_loop(
    state: Arc<BusLogState>,
    bus: Bus,
    participant_id: String,
    mut receiver: mpsc::Receiver<LogRecord>,
) {
    let topic = api::topic::internal::new(OwnerCap::__mint())
        .logs(&participant_id)
        .topic();
    let publisher = match Publisher::<api::logs::Event>::new(bus, &topic) {
        Ok(publisher) => publisher,
        Err(_) => return,
    };
    let mut seq = 0_u64;
    while let Some(record) = receiver.recv().await {
        let at = record.logical_time();
        let event = record.into_event(seq, state.take_dropped());
        seq = seq.wrapping_add(1);
        let result = IN_BUS_LOG_PUBLISH.with(|guard| {
            guard.set(true);
            let result = publisher.try_publish(at, event);
            guard.set(false);
            result
        });
        if result.is_err() {
            state.dropped.fetch_add(1, Ordering::Relaxed);
        }
    }
}

fn target_is_filtered(target: &str) -> bool {
    target.starts_with("zenoh")
        || target.starts_with("phoxal_bus")
        || target.starts_with("phoxal.bus")
}

fn level_to_gate(level: Level) -> u8 {
    match level {
        Level::ERROR => 1,
        Level::WARN => 2,
        Level::INFO => 3,
        Level::DEBUG => 4,
        Level::TRACE => 5,
    }
}

fn level_from_tracing(level: Level) -> api::logs::Level {
    match level {
        Level::ERROR => api::logs::Level::Error,
        Level::WARN => api::logs::Level::Warn,
        Level::INFO => api::logs::Level::Info,
        Level::DEBUG => api::logs::Level::Debug,
        Level::TRACE => api::logs::Level::Trace,
    }
}

fn parse_level(value: &str) -> Option<Level> {
    match value.trim().to_ascii_lowercase().as_str() {
        "error" => Some(Level::ERROR),
        "warn" | "warning" => Some(Level::WARN),
        "info" => Some(Level::INFO),
        "debug" => Some(Level::DEBUG),
        "trace" => Some(Level::TRACE),
        _ => None,
    }
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
        }
    }

    #[test]
    fn level_gate_defaults_to_info_plus() {
        let state = BusLogState::new();
        assert!(state.allows(Level::INFO, "app"));
        assert!(state.allows(Level::WARN, "app"));
        assert!(!state.allows(Level::DEBUG, "app"));

        state.set_max_level(Level::DEBUG);
        assert!(state.allows(Level::DEBUG, "app"));
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

    #[test]
    fn no_bus_attached_is_a_noop() {
        let state = BusLogState::new();
        state.try_enqueue(record());
        assert_eq!(state.take_dropped(), 0);
    }
}
