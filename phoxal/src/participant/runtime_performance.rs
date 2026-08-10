//! Runner-owned portable runtime-performance rollups.

use std::time::{Duration, Instant};

use phoxal_api::runtime;
use phoxal_bus::{
    BusHandle, RobotInstant, RuntimeBufferKind, RuntimeDirection, RuntimeMetricSnapshot,
    StreamPublisher,
};

use crate::participant::duration_nanos;
use crate::participant::scheduler::StepSchedule;

const ROLLUP_INTERVAL: Duration = Duration::from_secs(1);
const MAX_TOPIC_ROWS: usize = 256;
const MAX_TOPIC_BYTES: usize = 256;

pub(crate) struct RuntimePerformancePublisher {
    publisher: Option<StreamPublisher<runtime::endpoint::telemetry::TopicEndpoint>>,
}

impl RuntimePerformancePublisher {
    pub(crate) fn attach(bus: BusHandle) -> Self {
        let topic = runtime::topic::owner().telemetry().topic();
        let publisher = StreamPublisher::new(bus, &topic)
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

    pub(crate) fn publish(&self, body: runtime::telemetry::Rollup) {
        let Some(publisher) = &self.publisher else {
            return;
        };
        if let Err(error) = publisher.send(body) {
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
    next_rollup: Instant,
    step: Option<StepWindow>,
}

impl RuntimePerformance {
    pub(crate) fn new(schedule: Option<StepSchedule>) -> Self {
        Self::new_at(schedule, Instant::now())
    }

    fn new_at(schedule: Option<StepSchedule>, now: Instant) -> Self {
        Self {
            window_started: now,
            next_rollup: now + ROLLUP_INTERVAL,
            step: schedule.map(|schedule| StepWindow::new(schedule.period())),
        }
    }

    /// Start recording the step released for `target`. `None` for a participant
    /// with no cadence: there is no step window to record into.
    pub(crate) fn begin_step(
        &mut self,
        target: RobotInstant,
        fired_at: RobotInstant,
        missed_ticks: u32,
    ) -> Option<StepObservation> {
        self.step
            .as_ref()
            .map(|_| StepObservation::begin(target, fired_at, missed_ticks))
    }

    /// Drop cadence/history derived from the previous world history.
    pub(crate) fn reset(&mut self, schedule: Option<StepSchedule>) {
        *self = Self::new(schedule);
    }

    pub(crate) fn finish_step(&mut self, observation: Option<StepObservation>, success: bool) {
        if let (Some(step), Some(observation)) = (&mut self.step, observation) {
            step.finish(observation, Instant::now(), success);
        }
    }

    pub(crate) fn take_rollup(&mut self, bus: &BusHandle) -> Option<runtime::telemetry::Rollup> {
        let now = Instant::now();
        let elapsed = self.take_elapsed(now)?;
        let topics = TopicRows::from_snapshots(bus.take_runtime_metrics().ok()?, elapsed);
        Some(runtime::telemetry::Rollup {
            window_ns: duration_nanos(elapsed),
            step: self.step.as_mut().map(StepWindow::take),
            topics: topics.rows,
            overflow: topics.overflow,
        })
    }

    fn take_elapsed(&mut self, now: Instant) -> Option<Duration> {
        if now < self.next_rollup {
            return None;
        }
        let elapsed = now.saturating_duration_since(self.window_started);
        self.window_started = now;

        // Keep the cadence anchored to the original monotonic one-second grid.
        // A late poll advances directly to the first future grid line: it emits
        // one rollup covering the complete elapsed stall, never a catch-up burst.
        let overdue = now.saturating_duration_since(self.next_rollup);
        let remainder_ns = overdue.as_nanos() % ROLLUP_INTERVAL.as_nanos();
        let until_next_ns = ROLLUP_INTERVAL.as_nanos().saturating_sub(remainder_ns);
        let until_next = Duration::from_nanos(u64::try_from(until_next_ns).unwrap_or(u64::MAX));
        self.next_rollup = now + until_next;
        Some(elapsed)
    }
}

/// One step being measured, from release to return.
pub(crate) struct StepObservation {
    started: Instant,
    lateness: Duration,
    missed_ticks: u32,
}

impl StepObservation {
    /// Start measuring a step that was due at `target` and actually released at
    /// `fired_at`.
    ///
    /// A cross-timeline pair is not lateness at all - the world was replaced -
    /// so it records zero rather than an invented number.
    fn begin(target: RobotInstant, fired_at: RobotInstant, missed_ticks: u32) -> Self {
        StepObservation {
            started: Instant::now(),
            lateness: fired_at.duration_since(target).unwrap_or_default(),
            missed_ticks,
        }
    }
}

struct StepWindow {
    target_period: Duration,
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

    fn finish(&mut self, observation: StepObservation, finished: Instant, success: bool) {
        let duration = finished.saturating_duration_since(observation.started);
        let duration_ns = duration_nanos(duration);
        let lateness_ns = duration_nanos(observation.lateness);
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

    fn take(&mut self) -> runtime::telemetry::Step {
        let attempts = self.completed.saturating_add(self.errors);
        let body = runtime::telemetry::Step {
            target_period_ns: duration_nanos(self.target_period),
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

/// The per-topic section of one rollup, bounded for the wire.
///
/// One value rather than a pair, because the rows and the overflow row are one
/// answer: a row that does not fit is not dropped, it is folded into the
/// overflow row, so reading `rows` without `overflow` understates what the
/// window measured.
struct TopicRows {
    rows: Vec<runtime::telemetry::Topic>,
    overflow: Option<runtime::telemetry::Topic>,
}

impl TopicRows {
    /// Convert the window's snapshots, keeping at most [`MAX_TOPIC_ROWS`] rows
    /// of at most [`MAX_TOPIC_BYTES`] of topic identity each. Everything that
    /// does not fit is summed into a single overflow row, so an oversized or
    /// unbounded topic set costs a bounded number of bytes without hiding the
    /// traffic it carried.
    fn from_snapshots(snapshots: Vec<RuntimeMetricSnapshot>, elapsed: Duration) -> Self {
        let mut rows = Vec::with_capacity(snapshots.len().min(MAX_TOPIC_ROWS));
        let mut omitted = Vec::new();
        for snapshot in snapshots {
            let row = Self::wire_row(snapshot, elapsed);
            if rows.len() < MAX_TOPIC_ROWS && row.topic.len() <= MAX_TOPIC_BYTES {
                rows.push(row);
            } else {
                omitted.push(row);
            }
        }
        if omitted.is_empty() {
            return TopicRows {
                rows,
                overflow: None,
            };
        }
        let mut overflow = runtime::telemetry::Topic {
            // The overflow row is not one topic, one direction or one buffer, so
            // it names none of them: `Mixed` exists on the wire for exactly this
            // row and nothing measures it.
            topic: String::new(),
            direction: runtime::telemetry::Direction::Mixed,
            buffer_kind: runtime::telemetry::BufferKind::Mixed,
            count: 0,
            rate_millihz: 0,
            drops: 0,
            latest_overwrites: 0,
            bounded_evictions: 0,
            capacity: 0,
            current_depth: 0,
            high_water_depth: 0,
            decode_errors: 0,
            timeline_filtered: 0,
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
            overflow.timeline_filtered = overflow
                .timeline_filtered
                .saturating_add(row.timeline_filtered);
        }
        overflow.rate_millihz = rate_millihz(overflow.count, elapsed);
        TopicRows {
            rows,
            overflow: Some(overflow),
        }
    }

    /// One internal snapshot as the wire row a supervisor reads.
    ///
    /// `phoxal-bus` owns the internal accounting vocabulary and the runtime
    /// contract family owns its serialized representation; this is their only
    /// join.
    fn wire_row(snapshot: RuntimeMetricSnapshot, elapsed: Duration) -> runtime::telemetry::Topic {
        runtime::telemetry::Topic {
            topic: snapshot.key.topic,
            direction: match snapshot.key.direction {
                RuntimeDirection::Publish => runtime::telemetry::Direction::Publish,
                RuntimeDirection::Subscribe => runtime::telemetry::Direction::Subscribe,
            },
            buffer_kind: match snapshot.key.buffer_kind {
                RuntimeBufferKind::Outbound => runtime::telemetry::BufferKind::Outbound,
                RuntimeBufferKind::Latest => runtime::telemetry::BufferKind::Latest,
                RuntimeBufferKind::Subscriber => runtime::telemetry::BufferKind::Subscriber,
            },
            count: snapshot.count,
            rate_millihz: rate_millihz(snapshot.count, elapsed),
            drops: snapshot.drops,
            latest_overwrites: snapshot.latest_overwrites,
            bounded_evictions: snapshot.bounded_evictions,
            capacity: snapshot.capacity,
            current_depth: snapshot.current_depth,
            high_water_depth: snapshot.high_water_depth,
            decode_errors: snapshot.decode_errors,
            timeline_filtered: snapshot.timeline_filtered,
            overflowed_rows: 0,
        }
    }
}

fn rate_millihz(count: u64, elapsed: Duration) -> u64 {
    if elapsed.is_zero() {
        return 0;
    }
    count
        .saturating_mul(1_000_000_000_000)
        .saturating_div(u64::try_from(elapsed.as_nanos()).unwrap_or(u64::MAX))
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
                topic: format!("robot/test/{index:03}"),
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
            timeline_filtered: 0,
        }
    }

    #[test]
    fn topic_rows_are_deterministic_and_cap_at_256_plus_overflow() {
        let snapshots = (0..260).map(row).collect();
        let topics = TopicRows::from_snapshots(snapshots, Duration::from_secs(1));
        assert_eq!(topics.rows.len(), MAX_TOPIC_ROWS);
        assert_eq!(topics.rows.first().unwrap().topic, "robot/test/000");
        assert_eq!(topics.rows.last().unwrap().topic, "robot/test/255");
        let overflow = topics.overflow.expect("four rows should overflow");
        assert_eq!(overflow.overflowed_rows, 4);
        assert_eq!(overflow.count, 4);
    }

    #[test]
    fn oversized_topic_identity_is_disclosed_in_overflow_not_put_on_wire() {
        let mut oversized = row(0);
        oversized.key.topic = "x".repeat(MAX_TOPIC_BYTES + 1);
        let topics = TopicRows::from_snapshots(vec![oversized], Duration::from_secs(1));
        assert!(topics.rows.is_empty());
        assert_eq!(topics.overflow.unwrap().overflowed_rows, 1);
    }

    #[test]
    fn unscheduled_steps_are_not_applicable() {
        let performance = RuntimePerformance::new(None);
        assert!(performance.step.is_none());
    }

    #[test]
    fn rollup_gate_emits_at_most_once_per_host_monotonic_second() {
        let started = Instant::now();
        let mut performance = RuntimePerformance::new_at(None, started);
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
    fn rollup_grid_survives_alternating_jitter_and_collapses_long_stalls() {
        let started = Instant::now();
        let mut performance = RuntimePerformance::new_at(None, started);

        assert_eq!(
            performance.take_elapsed(started + Duration::from_millis(900)),
            None
        );
        assert_eq!(
            performance.take_elapsed(started + Duration::from_millis(1_100)),
            Some(Duration::from_millis(1_100))
        );
        assert_eq!(
            performance.take_elapsed(started + Duration::from_millis(1_900)),
            None
        );
        assert_eq!(
            performance.take_elapsed(started + Duration::from_millis(2_100)),
            Some(Duration::from_secs(1))
        );
        assert_eq!(
            performance.take_elapsed(started + Duration::from_millis(2_900)),
            None
        );
        assert_eq!(
            performance.take_elapsed(started + Duration::from_millis(3_100)),
            Some(Duration::from_secs(1))
        );

        // Five grid lines elapsed, but one rollup covers the whole stall and a
        // second call at the same instant cannot produce catch-up samples.
        let after_stall = started + Duration::from_millis(8_400);
        assert_eq!(
            performance.take_elapsed(after_stall),
            Some(Duration::from_millis(5_300))
        );
        assert_eq!(performance.take_elapsed(after_stall), None);
        assert_eq!(performance.next_rollup, started + Duration::from_secs(9));
    }

    #[test]
    fn lateness_uses_fired_at_minus_target_independently_of_missed_ticks() {
        let schedule = StepSchedule::hz(100.0);
        let mut performance = RuntimePerformance::new(Some(schedule));
        let line = phoxal_bus::TimelineId::mint();
        let observation = performance
            .begin_step(
                RobotInstant::new(line, 100),
                RobotInstant::new(line, 135),
                7,
            )
            .expect("scheduled participant has step observation");
        assert_eq!(observation.lateness, Duration::from_nanos(35));
        assert_eq!(observation.missed_ticks, 7);
    }

    #[test]
    fn step_window_rolls_up_success_error_lateness_misses_and_overrun() {
        let period = Duration::from_millis(10);
        let mut window = StepWindow::new(period);
        let start = Instant::now();
        let first = StepObservation {
            started: start,
            lateness: Duration::from_millis(2),
            missed_ticks: 0,
        };
        window.finish(first, start + Duration::from_millis(4), true);
        let second_start = start + Duration::from_millis(25);
        let second = StepObservation {
            started: second_start,
            lateness: Duration::from_millis(5),
            missed_ticks: 2,
        };
        window.finish(second, second_start + Duration::from_millis(12), false);

        let sample = window.take();
        assert_eq!(sample.completed, 1);
        assert_eq!(sample.errors, 1);
        assert_eq!(sample.mean_duration_ns, 8_000_000);
        assert_eq!(sample.max_duration_ns, 12_000_000);
        assert_eq!(sample.mean_lateness_ns, 3_500_000);
        assert_eq!(sample.max_lateness_ns, 5_000_000);
        assert_eq!(sample.missed_ticks, 2);
        assert_eq!(sample.overruns, 1);
    }
}
