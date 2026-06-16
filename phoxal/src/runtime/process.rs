use std::time::Duration;

use anyhow::Result;

use crate::api::presence::{self, v1::Readiness};
use crate::api::topic;
use crate::bus::Bus;
use crate::runtime::Runtime;
use crate::runtime::clock::StepStream;
use crate::runtime::input::collect_step_inputs;
use crate::runtime::io::Io;

pub struct RuntimeProcess<'a> {
    bus: &'a Bus,
    simulation: bool,
    period: Duration,
}

impl<'a> RuntimeProcess<'a> {
    pub const fn new(bus: &'a Bus, simulation: bool, period: Duration) -> Self {
        Self {
            bus,
            simulation,
            period,
        }
    }

    pub async fn run<R>(self, config: R::Config) -> Result<()>
    where
        R: Runtime,
    {
        tracing::info!(
            runtime = R::RUNTIME_ID,
            simulation = self.simulation,
            period_ms = self.period.as_millis() as u64,
            "runtime starting"
        );

        let mut io = Io::live((*self.bus).clone(), R::RUNTIME_ID);
        let mut runtime = R::new(&mut io, config).await?;
        let mut steps = StepStream::new(self.bus, self.simulation, self.period).await?;
        let heartbeat_pub = io
            .publisher_topic(topic::new().presence().heartbeat())
            .await?;
        let (handles, sources) = io.into_parts();
        let _handles = handles;

        tracing::info!(runtime = R::RUNTIME_ID, "runtime ready");

        let mut last_heartbeat_ns = None;
        let mut steps_in_window = 0_u64;
        let mut inputs_in_window = 0_u64;
        let mut dropped_in_window = 0_u64;

        loop {
            tokio::select! {
                result = tokio::signal::ctrl_c() => {
                    result?;
                    tracing::info!(runtime = R::RUNTIME_ID, "runtime shutting down");
                    runtime.shutdown().await?;
                    break;
                }
                result = steps.next() => {
                    let step = result?;
                    let inputs = collect_step_inputs(&sources)?;
                    let stats = inputs.stats();
                    runtime.step(step, inputs).await?;
                    let step_time_ns = step.tick.time_ns();
                    let window_start_ns = last_heartbeat_ns.get_or_insert(step_time_ns);
                    steps_in_window = steps_in_window.saturating_add(1);
                    inputs_in_window = inputs_in_window.saturating_add(stats.delivered);
                    dropped_in_window = dropped_in_window.saturating_add(stats.dropped);
                    let elapsed_ns = step_time_ns.saturating_sub(*window_start_ns);
                    if elapsed_ns >= 10_000_000_000 {
                        tracing::info!(
                            runtime = R::RUNTIME_ID,
                            steps = steps_in_window,
                            inputs = inputs_in_window,
                            dropped = dropped_in_window,
                            window_s = elapsed_ns as f64 / 1_000_000_000_f64,
                            "runtime alive"
                        );
                        last_heartbeat_ns = Some(step_time_ns);
                        steps_in_window = 0;
                        inputs_in_window = 0;
                        dropped_in_window = 0;
                    }
                    heartbeat_pub
                        .put(
                            step.tick.time_ns(),
                            &presence::Heartbeat::V1(presence::v1::Heartbeat {
                                runtime_id: presence::v1::RuntimeId::new(R::RUNTIME_ID),
                                readiness: Readiness::Ready,
                            }),
                        )
                        .await?;
                }
            }
        }

        Ok(())
    }
}
