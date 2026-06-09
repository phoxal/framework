use std::time::Duration;

use crate::api::simulation::v1::clock::{self, Clock};
use crate::bus::Bus;
use crate::bus::pubsub::Stamped;
use crate::bus::zenoh::TypedSubscriber;
use anyhow::{Result, anyhow};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Step {
    pub tick: Clock,
}

impl Step {
    pub const fn new(tick: Clock) -> Self {
        Self { tick }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchedulePolicy {
    CatchUp,
    Collapse,
    Skip,
}

#[derive(Debug, Clone)]
pub struct Schedule {
    period_ns: u64,
    policy: SchedulePolicy,
    next_deadline_ns: u64,
}

impl Schedule {
    pub const fn new(period_ns: u64, policy: SchedulePolicy) -> Self {
        Self {
            period_ns,
            policy,
            next_deadline_ns: period_ns,
        }
    }

    pub const fn from_publish_hz(publish_hz: f64, policy: SchedulePolicy) -> Self {
        Self::new((1_000_000_000_f64 / publish_hz) as u64, policy)
    }

    pub fn due_steps(&mut self, time_ns: u64) -> u64 {
        if self.period_ns == 0 || time_ns < self.next_deadline_ns {
            return 0;
        }

        let overdue = time_ns - self.next_deadline_ns;
        let missed = overdue / self.period_ns;
        let due = match self.policy {
            SchedulePolicy::CatchUp => missed + 1,
            SchedulePolicy::Collapse | SchedulePolicy::Skip => 1,
        };

        self.next_deadline_ns = match self.policy {
            SchedulePolicy::CatchUp | SchedulePolicy::Collapse => {
                self.next_deadline_ns + ((missed + 1) * self.period_ns)
            }
            SchedulePolicy::Skip => time_ns + self.period_ns,
        };

        due
    }
}

#[derive(Debug)]
struct RealClock {
    step: u64,
    time_ns: u64,
    dt_ns: u64,
    interval: tokio::time::Interval,
}

impl RealClock {
    fn new(period: Duration) -> Self {
        let mut interval = tokio::time::interval(period);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        Self {
            step: 0,
            time_ns: 0,
            dt_ns: period.as_nanos() as u64,
            interval,
        }
    }

    async fn tick(&mut self) -> Step {
        self.interval.tick().await;
        self.step = self.step.saturating_add(1);
        self.time_ns = self.time_ns.saturating_add(self.dt_ns);
        Step::new(Clock::new(0, self.step, self.time_ns, self.dt_ns))
    }
}

enum StepSource {
    Local(RealClock),
    Simulation(TypedSubscriber<Stamped<Clock>>),
}

pub(crate) struct StepStream {
    source: StepSource,
    bound_epoch: Option<u64>,
    last_step: Option<u64>,
}

impl StepStream {
    pub(crate) async fn new(bus: &Bus, simulation: bool, period: Duration) -> Result<Self> {
        Ok(Self {
            source: if simulation {
                StepSource::Simulation(
                    clock::subscriber_builder(bus)
                        .await
                        .map_err(crate::bus::Error::from)?,
                )
            } else {
                StepSource::Local(RealClock::new(period))
            },
            bound_epoch: None,
            last_step: None,
        })
    }

    pub(crate) async fn next(&mut self) -> Result<Step> {
        loop {
            return match &mut self.source {
                StepSource::Local(clock) => Ok(clock.tick().await),
                StepSource::Simulation(subscriber) => match subscriber.recv_async().await {
                    Ok(Ok(stamped)) => {
                        let tick = stamped.data;
                        if self.bound_epoch != Some(tick.epoch()) {
                            self.bound_epoch = Some(tick.epoch());
                            self.last_step = None;
                        }

                        if let Some(last_step) = self.last_step
                            && tick.step() <= last_step
                        {
                            continue;
                        }

                        self.last_step = Some(tick.step());
                        Ok(Step::new(tick))
                    }
                    Ok(Err(error)) => {
                        Err(anyhow!("simulation clock payload decode failed: {error}"))
                    }
                    Err(error) => Err(anyhow!("simulation clock subscription failed: {error}")),
                },
            };
        }
    }
}
