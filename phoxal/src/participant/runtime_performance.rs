//! Runner-owned portable runtime-performance rollups.

use std::time::{Duration, Instant};

use phoxal_api::v1 as api;
use phoxal_bus::{
    Bus, LogicalTime, OwnerCap, Publisher, RuntimeBufferKind, RuntimeDirection,
    RuntimeMetricSnapshot,
};

use crate::participant::spec::StepSchedule;

const ROLLUP_INTERVAL: Duration = Duration::from_secs(1);
const MAX_TOPIC_ROWS: usize = 256;
const MAX_TOPIC_BYTES: usize = 256;

pub(crate) struct RuntimePerformancePublisher {
    publisher: Option<Publisher<api::tool::runtime::Rollup>>,
}

impl RuntimePerformancePublisher {
    pub(crate) fn attach(bus: Bus) -> Self {
        let topic = api::topic::internal::new(OwnerCap::__mint())
            .tool()
            .runtime()
            .rollup();
        let publisher = Publisher::new(bus, &topic)
            .inspect_err(|error| {
                tracing::warn!(
                    target: "phoxal.runtime",
                    error = %error,
                    "runtime-performance publisher could not be created"
                );
            })
            .ok();
        Self { publisher }
    }

    pub(crate) fn publish(&self, at: LogicalTime, body: api::tool::runtime::Rollup) {
        let Some(publisher) = &self.publisher else {
            return;
        };
        if let Err(error) = publisher.try_publish(at, body) {
            tracing::warn!(
                target: "phoxal.runtime",
                error = %error,
                "runtime-performance publish failed"
            );
        }
    }
}

pub(crate) struct RuntimePerformance {
    window_started: Instant,
    step: Option<StepWindow>,
}

impl RuntimePerformance {
    pub(crate) fn new(schedule: Option<StepSchedule>) -> Self {
        Self {
            window_started: Instant::now(),
            step: schedule.map(|schedule| StepWindow::new(schedule.period())),
        }
    }

    pub(crate) fn begin_step(&mut self, missed_ticks: u32) -> Option<StepObservation> {
        self.step
            .as_mut()
            .map(|step| step.begin(Instant::now(), missed_ticks))
    }

    pub(crate) fn finish_step(&mut self, observation: Option<StepObservation>, success: bool) {
        if let (Some(step), Some(observation)) = (&mut self.step, observation) {
            step.finish(observation, Instant::now(), success);
        }
    }

    pub(crate) fn take_rollup(&mut self, bus: &Bus) -> Option<api::tool::runtime::Rollup> {
        let now = Instant::now();
        let elapsed = self.take_elapsed(now)?;
        let window_ns = nanos(elapsed);
        let (topics, overflow) = bounded_topics(bus.take_runtime_metrics(), elapsed);
        Some(api::tool::runtime::Rollup {
            window_ns,
            step: self.step.as_mut().map(StepWindow::take),
            topics,
            overflow,
        })
    }

    fn take_elapsed(&mut self, now: Instant) -> Option<Duration> {
        let elapsed = now.saturating_duration_since(self.window_started);
        if elapsed < ROLLUP_INTERVAL {
            return None;
        }
        self.window_started = now;
        Some(elapsed)
    }
}

pub(crate) struct StepObservation {
    started: Instant,
    lateness: Duration,
    missed_ticks: u32,
}

struct StepWindow {
    target_period: Duration,
    previous_started: Option<Instant>,
    completed: u64,
    errors: u64,
    duration_total_ns: u128,
    duration_max_ns: u64,
    lateness_total_ns: u128,
    lateness_max_ns: u64,
    missed_ticks: u64,
    overruns: u64,
}

impl StepWindow {
    fn new(target_period: Duration) -> Self {
        Self {
            target_period,
            previous_started: None,
            completed: 0,
            errors: 0,
            duration_total_ns: 0,
            duration_max_ns: 0,
            lateness_total_ns: 0,
            lateness_max_ns: 0,
            missed_ticks: 0,
            overruns: 0,
        }
    }

    fn begin(&mut self, started: Instant, missed_ticks: u32) -> StepObservation {
        let lateness = self
            .previous_started
            .map(|previous| {
                started
                    .saturating_duration_since(previous)
                    .saturating_sub(self.target_period)
            })
            .unwrap_or_default();
        self.previous_started = Some(started);
        StepObservation {
            started,
            lateness,
            missed_ticks,
        }
    }

    fn finish(&mut self, observation: StepObservation, finished: Instant, success: bool) {
        let duration = finished.saturating_duration_since(observation.started);
        let duration_ns = nanos(duration);
        let lateness_ns = nanos(observation.lateness);
        if success {
            self.completed = self.completed.saturating_add(1);
        } else {
            self.errors = self.errors.saturating_add(1);
        }
        self.duration_total_ns = self.duration_total_ns.saturating_add(duration.as_nanos());
        self.duration_max_ns = self.duration_max_ns.max(duration_ns);
        self.lateness_total_ns = self
            .lateness_total_ns
            .saturating_add(observation.lateness.as_nanos());
        self.lateness_max_ns = self.lateness_max_ns.max(lateness_ns);
        self.missed_ticks = self
            .missed_ticks
            .saturating_add(u64::from(observation.missed_ticks));
        if duration > self.target_period {
            self.overruns = self.overruns.saturating_add(1);
        }
    }

    fn take(&mut self) -> api::tool::RuntimeStep {
        let attempts = self.completed.saturating_add(self.errors);
        let body = api::tool::RuntimeStep {
            target_period_ns: nanos(self.target_period),
            completed: self.completed,
            errors: self.errors,
            mean_duration_ns: mean(self.duration_total_ns, attempts),
            max_duration_ns: self.duration_max_ns,
            mean_lateness_ns: mean(self.lateness_total_ns, attempts),
            max_lateness_ns: self.lateness_max_ns,
            missed_ticks: self.missed_ticks,
            overruns: self.overruns,
        };
        self.completed = 0;
        self.errors = 0;
        self.duration_total_ns = 0;
        self.duration_max_ns = 0;
        self.lateness_total_ns = 0;
        self.lateness_max_ns = 0;
        self.missed_ticks = 0;
        self.overruns = 0;
        body
    }
}

fn bounded_topics(
    rows: Vec<RuntimeMetricSnapshot>,
    elapsed: Duration,
) -> (
    Vec<api::tool::RuntimeTopic>,
    Option<api::tool::RuntimeTopic>,
) {
    let mut converted = Vec::with_capacity(rows.len().min(MAX_TOPIC_ROWS));
    let mut omitted = Vec::new();
    for row in rows {
        let row = topic_row(row, elapsed);
        if converted.len() < MAX_TOPIC_ROWS && row.topic.len() <= MAX_TOPIC_BYTES {
            converted.push(row);
        } else {
            omitted.push(row);
        }
    }
    if omitted.is_empty() {
        return (converted, None);
    }
    let mut overflow = api::tool::RuntimeTopic {
        topic: String::new(),
        direction: api::tool::RuntimeDirection::Mixed,
        buffer_kind: api::tool::RuntimeBufferKind::Mixed,
        count: 0,
        rate_hz: 0.0,
        drops: 0,
        latest_overwrites: 0,
        bounded_evictions: 0,
        capacity: 0,
        current_depth: 0,
        high_water_depth: 0,
        decode_errors: 0,
        overflowed_rows: u32::try_from(omitted.len()).unwrap_or(u32::MAX),
    };
    for row in omitted {
        overflow.count = overflow.count.saturating_add(row.count);
        overflow.drops = overflow.drops.saturating_add(row.drops);
        overflow.latest_overwrites = overflow
            .latest_overwrites
            .saturating_add(row.latest_overwrites);
        overflow.bounded_evictions = overflow
            .bounded_evictions
            .saturating_add(row.bounded_evictions);
        overflow.capacity = overflow.capacity.saturating_add(row.capacity);
        overflow.current_depth = overflow.current_depth.saturating_add(row.current_depth);
        overflow.high_water_depth = overflow
            .high_water_depth
            .saturating_add(row.high_water_depth);
        overflow.decode_errors = overflow.decode_errors.saturating_add(row.decode_errors);
    }
    overflow.rate_hz = rate(overflow.count, elapsed);
    (converted, Some(overflow))
}

fn topic_row(row: RuntimeMetricSnapshot, elapsed: Duration) -> api::tool::RuntimeTopic {
    api::tool::RuntimeTopic {
        topic: row.key.topic,
        direction: match row.key.direction {
            RuntimeDirection::Publish => api::tool::RuntimeDirection::Publish,
            RuntimeDirection::Subscribe => api::tool::RuntimeDirection::Subscribe,
        },
        buffer_kind: match row.key.buffer_kind {
            RuntimeBufferKind::Outbound => api::tool::RuntimeBufferKind::Outbound,
            RuntimeBufferKind::Latest => api::tool::RuntimeBufferKind::Latest,
            RuntimeBufferKind::Subscriber => api::tool::RuntimeBufferKind::Subscriber,
        },
        count: row.count,
        rate_hz: rate(row.count, elapsed),
        drops: row.drops,
        latest_overwrites: row.latest_overwrites,
        bounded_evictions: row.bounded_evictions,
        capacity: row.capacity,
        current_depth: row.current_depth,
        high_water_depth: row.high_water_depth,
        decode_errors: row.decode_errors,
        overflowed_rows: 0,
    }
}

fn rate(count: u64, elapsed: Duration) -> f32 {
    if elapsed.is_zero() {
        return 0.0;
    }
    (count as f64 / elapsed.as_secs_f64()) as f32
}

fn nanos(duration: Duration) -> u64 {
    u64::try_from(duration.as_nanos()).unwrap_or(u64::MAX)
}

fn mean(total: u128, count: u64) -> u64 {
    if count == 0 {
        0
    } else {
        u64::try_from(total / u128::from(count)).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal_bus::{RuntimeMetricKey, RuntimeMetricSnapshot};

    fn row(index: usize) -> RuntimeMetricSnapshot {
        RuntimeMetricSnapshot {
            key: RuntimeMetricKey {
                topic: format!("v1/test/{index:03}"),
                direction: RuntimeDirection::Publish,
                buffer_kind: RuntimeBufferKind::Outbound,
            },
            count: 1,
            drops: 0,
            latest_overwrites: 0,
            bounded_evictions: 0,
            capacity: 1,
            current_depth: 0,
            high_water_depth: 1,
            decode_errors: 0,
        }
    }

    #[test]
    fn topic_rows_are_deterministic_and_cap_at_256_plus_overflow() {
        let rows = (0..260).map(row).collect();
        let (topics, overflow) = bounded_topics(rows, Duration::from_secs(1));
        assert_eq!(topics.len(), MAX_TOPIC_ROWS);
        assert_eq!(topics.first().unwrap().topic, "v1/test/000");
        assert_eq!(topics.last().unwrap().topic, "v1/test/255");
        let overflow = overflow.expect("four rows should overflow");
        assert_eq!(overflow.overflowed_rows, 4);
        assert_eq!(overflow.count, 4);
    }

    #[test]
    fn oversized_topic_identity_is_disclosed_in_overflow_not_put_on_wire() {
        let mut oversized = row(0);
        oversized.key.topic = "x".repeat(MAX_TOPIC_BYTES + 1);
        let (topics, overflow) = bounded_topics(vec![oversized], Duration::from_secs(1));
        assert!(topics.is_empty());
        assert_eq!(overflow.unwrap().overflowed_rows, 1);
    }

    #[test]
    fn unscheduled_steps_are_not_applicable() {
        let performance = RuntimePerformance::new(None);
        assert!(performance.step.is_none());
    }

    #[test]
    fn rollup_gate_emits_at_most_once_per_host_monotonic_second() {
        let mut performance = RuntimePerformance::new(None);
        let started = performance.window_started;
        assert_eq!(
            performance.take_elapsed(started + Duration::from_millis(999)),
            None
        );
        assert_eq!(
            performance.take_elapsed(started + Duration::from_secs(1)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            performance.take_elapsed(started + Duration::from_secs(1)),
            None
        );
    }

    #[test]
    fn step_window_rolls_up_success_error_lateness_misses_and_overrun() {
        let period = Duration::from_millis(10);
        let mut window = StepWindow::new(period);
        let start = Instant::now();
        let first = window.begin(start, 0);
        window.finish(first, start + Duration::from_millis(4), true);
        let second_start = start + Duration::from_millis(25);
        let second = window.begin(second_start, 2);
        window.finish(second, second_start + Duration::from_millis(12), false);

        let sample = window.take();
        assert_eq!(sample.completed, 1);
        assert_eq!(sample.errors, 1);
        assert_eq!(sample.mean_duration_ns, 8_000_000);
        assert_eq!(sample.max_duration_ns, 12_000_000);
        assert_eq!(sample.mean_lateness_ns, 7_500_000);
        assert_eq!(sample.max_lateness_ns, 15_000_000);
        assert_eq!(sample.missed_ticks, 2);
        assert_eq!(sample.overruns, 1);
    }
}
