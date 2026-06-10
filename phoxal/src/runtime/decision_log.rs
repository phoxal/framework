//! Runtime decision logging contract.
//!
//! A runtime owns the typed key that describes its decision semantics, usually
//! derived from the runtime's `State` payload. The engine owns the logging
//! mechanics: a single stable structured tracing event, emitted only when that
//! typed key changes versus the last emitted key and bounded by logical time.
//! Runtime crates should call [`DecisionLog::observe`] once per step with their
//! current typed key instead of hand-rolling per-runtime `last_logged_state`
//! fields or free-text event names.

use std::fmt::Debug;

use crate::bus::topic::{PubSub, Topic};

/// Stable tracing target for all runtime decision-change events.
pub const DECISION_LOG_TARGET: &str = "phoxal.runtime.decision";

/// Default logical-time bound between decision-change log events.
pub const DEFAULT_DECISION_LOG_MIN_INTERVAL_NS: u64 = 1_000_000_000;

/// Structured description of a decision-change event emitted by [`DecisionLog`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DecisionLogEmission<K> {
    pub runtime_id: &'static str,
    pub decision_label: String,
    pub schema_name: &'static str,
    pub schema_version: u32,
    pub key: K,
    pub now_ns: u64,
    pub suppressed_count: u64,
}

/// Engine-owned helper for bounded runtime decision-change tracing.
#[derive(Debug, Clone)]
pub struct DecisionLog<K> {
    runtime_id: &'static str,
    decision_label: String,
    schema_name: &'static str,
    schema_version: u32,
    min_interval_ns: u64,
    last_emitted_key: Option<K>,
    last_observed_key: Option<K>,
    last_emit_ns: Option<u64>,
    suppressed_count: u64,
}

impl<K> DecisionLog<K>
where
    K: PartialEq + Clone + Debug,
{
    /// Creates a decision logger for one runtime-owned typed decision key.
    ///
    /// `runtime_id` is the owning runtime's stable id, `decision_label` is the
    /// stable label operators use to identify the decision surface (normally the
    /// runtime state topic), and `schema_name` / `schema_version` identify the
    /// typed contract the key was derived from.
    pub fn new(
        runtime_id: &'static str,
        decision_label: impl Into<String>,
        schema_name: &'static str,
        schema_version: u32,
    ) -> Self {
        Self {
            runtime_id,
            decision_label: decision_label.into(),
            schema_name,
            schema_version,
            min_interval_ns: DEFAULT_DECISION_LOG_MIN_INTERVAL_NS,
            last_emitted_key: None,
            last_observed_key: None,
            last_emit_ns: None,
            suppressed_count: 0,
        }
    }

    /// Creates a decision logger whose label and schema identity come from a
    /// fluent typed topic.
    pub fn from_topic<T>(runtime_id: &'static str, topic: &Topic<PubSub<T>>) -> Self {
        Self::new(
            runtime_id,
            topic.key().into_owned(),
            topic.schema(),
            topic.version(),
        )
    }

    /// Overrides the logical-time bound between emitted decision log events.
    pub fn with_min_interval_ns(mut self, min_interval_ns: u64) -> Self {
        self.min_interval_ns = min_interval_ns;
        self
    }

    /// Observes the current typed decision key and emits only when the contract
    /// allows a new structured decision-change event.
    ///
    /// The initial key always emits. After that, repeated identical keys are
    /// suppressed. Key changes inside `min_interval_ns` since the previous emit
    /// increment `suppressed_count` once per observed key transition and fold
    /// into the next emitted event.
    pub fn observe(&mut self, now_ns: u64, key: K) -> Option<DecisionLogEmission<K>> {
        if self.last_emitted_key.is_none() {
            return Some(self.emit(now_ns, key, 0));
        }

        if self.last_observed_key.as_ref() == Some(&key) {
            if self.last_emitted_key.as_ref() == Some(&key) {
                return None;
            }
            if self.can_emit(now_ns) {
                let suppressed_count = self.suppressed_count;
                return Some(self.emit(now_ns, key, suppressed_count));
            }
            return None;
        }

        if self.last_emitted_key.as_ref() == Some(&key) {
            self.last_observed_key = Some(key);
            self.suppressed_count = self.suppressed_count.saturating_add(1);
            return None;
        }

        if self.can_emit(now_ns) {
            let suppressed_count = self.suppressed_count;
            return Some(self.emit(now_ns, key, suppressed_count));
        }

        self.last_observed_key = Some(key);
        self.suppressed_count = self.suppressed_count.saturating_add(1);
        None
    }

    fn can_emit(&self, now_ns: u64) -> bool {
        self.last_emit_ns
            .is_none_or(|last_emit_ns| now_ns.saturating_sub(last_emit_ns) >= self.min_interval_ns)
    }

    fn emit(&mut self, now_ns: u64, key: K, suppressed_count: u64) -> DecisionLogEmission<K> {
        tracing::info!(
            target: DECISION_LOG_TARGET,
            runtime_id = self.runtime_id,
            decision_label = self.decision_label.as_str(),
            schema_name = self.schema_name,
            schema_version = self.schema_version,
            decision_key = ?key,
            now_ns,
            suppressed_count,
            "runtime decision changed"
        );

        self.last_emitted_key = Some(key.clone());
        self.last_observed_key = Some(key.clone());
        self.last_emit_ns = Some(now_ns);
        self.suppressed_count = 0;

        DecisionLogEmission {
            runtime_id: self.runtime_id,
            decision_label: self.decision_label.clone(),
            schema_name: self.schema_name,
            schema_version: self.schema_version,
            key,
            now_ns,
            suppressed_count,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DecisionLog, DecisionLogEmission};

    const MIN_INTERVAL_NS: u64 = 100;

    #[derive(Clone, Copy, Debug, PartialEq, Eq)]
    enum Key {
        A,
        B,
        C,
    }

    fn decision_log() -> DecisionLog<Key> {
        DecisionLog::new(
            "test-runtime",
            "runtime/test/state",
            "runtime/test/state",
            7,
        )
        .with_min_interval_ns(MIN_INTERVAL_NS)
    }

    fn emission(key: Key, now_ns: u64, suppressed_count: u64) -> DecisionLogEmission<Key> {
        DecisionLogEmission {
            runtime_id: "test-runtime",
            decision_label: "runtime/test/state".to_string(),
            schema_name: "runtime/test/state",
            schema_version: 7,
            key,
            now_ns,
            suppressed_count,
        }
    }

    #[test]
    fn from_topic_uses_typed_topic_identity() {
        let topic = crate::api::v1::topic::new().v1().motion().state();
        let mut log = DecisionLog::from_topic("motion", &topic);

        assert_eq!(
            log.observe(10, Key::A),
            Some(DecisionLogEmission {
                runtime_id: "motion",
                decision_label: "v1/motion/state".to_string(),
                schema_name: "v1/motion/state",
                schema_version: 1,
                key: Key::A,
                now_ns: 10,
                suppressed_count: 0,
            })
        );
    }

    #[test]
    fn initial_key_emits() {
        let mut log = decision_log();

        assert_eq!(log.observe(10, Key::A), Some(emission(Key::A, 10, 0)));
    }

    #[test]
    fn identical_key_suppresses() {
        let mut log = decision_log();

        assert_eq!(log.observe(10, Key::A), Some(emission(Key::A, 10, 0)));
        assert_eq!(log.observe(110, Key::A), None);
    }

    #[test]
    fn change_inside_min_interval_accumulates_suppressed_count_until_window_opens() {
        let mut log = decision_log();

        assert_eq!(log.observe(0, Key::A), Some(emission(Key::A, 0, 0)));
        assert_eq!(log.observe(10, Key::B), None);
        assert_eq!(log.observe(20, Key::B), None);
        assert_eq!(log.observe(30, Key::C), None);
        assert_eq!(log.observe(100, Key::C), Some(emission(Key::C, 100, 2)));
    }

    #[test]
    fn distinct_keys_across_windows_each_emit() {
        let mut log = decision_log();

        assert_eq!(log.observe(0, Key::A), Some(emission(Key::A, 0, 0)));
        assert_eq!(log.observe(100, Key::B), Some(emission(Key::B, 100, 0)));
        assert_eq!(log.observe(200, Key::C), Some(emission(Key::C, 200, 0)));
    }
}
