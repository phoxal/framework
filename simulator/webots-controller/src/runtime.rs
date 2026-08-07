//! The external Webots step loop.
//!
//! Webots owns the cadence, so this controller runs no framework step loop of
//! its own. Each iteration applies the actuator inputs, advances the world one
//! step, and commits everything that step produced.

use anyhow::Result;
// `TimelineAuthority` is deliberately not part of `phoxal::bus`/`phoxal::prelude`:
// it is world-clock authority, which only this simulator legitimately names, so
// it lives behind the explicit `phoxal_bus` opt-in instead - see that module's
// docs.
use phoxal_bus::TimelineAuthority;

use crate::backend::SharedBackend;
use crate::controller::Api;

pub(crate) struct ControllerRuntime {
    /// This controller's exclusive ownership of the world's timeline. It is
    /// the only way anything in this process can express a robot instant.
    authority: TimelineAuthority,
    step_index: u64,
    backend: SharedBackend,
}

impl ControllerRuntime {
    pub(crate) const fn new(authority: TimelineAuthority, backend: SharedBackend) -> Self {
        Self {
            authority,
            step_index: 0,
            backend,
        }
    }

    /// Step the world until it stops. A world that stopped is left quiet
    /// before the error is reported, so a failed step never leaves the motors
    /// running.
    pub(crate) async fn run(mut self, api: Api) -> Result<()> {
        loop {
            if let Err(error) = self.step_once(&api).await {
                if let Err(park_error) = self.backend.park().await {
                    tracing::warn!(
                        target: "simulator_webots_controller",
                        error = %park_error,
                        "failed to quiet the world after the Webots step loop stopped"
                    );
                }
                return Err(error);
            }
        }
    }

    async fn step_once(&mut self, api: &Api) -> Result<()> {
        let next_step = self.step_index.saturating_add(1);
        let (time_ns, output) = self.backend.advance(self.step_index, next_step).await?;

        // One completed world advance mints one token, and every output of
        // that advance is stamped with it. There is no other way for this
        // process to express a robot instant.
        let world_step = self.authority.completed_step(time_ns);
        api.commit_step(&world_step, next_step, output)?;
        self.step_index = next_step;
        tracing::trace!(
            target: "simulator_webots_controller",
            timeline = %self.authority.timeline(),
            step = self.step_index,
            ticks = time_ns,
            "external Webots step committed"
        );
        Ok(())
    }
}
