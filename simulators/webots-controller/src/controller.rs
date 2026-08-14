//! The Webots controller simulator participant.
//!
//! Binds one Webots-owned controller process to a robot's component
//! capabilities and publishes or subscribes exactly the `component::*`
//! contracts those capabilities need. The process bootstraps the normal
//! framework runner, mints one opaque timeline, and runs only the external
//! Webots step loop.
//!
//! Every capability kind a component may declare is simulated except one:
//! Webots has no button, switch, or toggle node, so nothing in a simulated
//! world can engage or release an `emergency_stop`. That capability is
//! deliberately left unpublished rather than driven from a static config,
//! which would assert a state no one in the world can change.

use anyhow::Result;
use phoxal::bus::{TimelineId, WorldStepToken};
use phoxal::prelude::*;
// `WorldClockPublisher` is deliberately not part of `phoxal::bus`/`phoxal::prelude`:
// it is world-clock authority, which only this simulator legitimately names, so
// it lives behind the explicit `phoxal_bus` opt-in instead - see that module's
// docs.
use phoxal_bus::WorldClockPublisher;
use phoxal_protocol::runtime::endpoint::simulation::ClockEndpoint;
use phoxal_protocol::runtime::simulation::Clock;

use crate::backend::{SharedBackend, WebotsHandle};
use crate::catalog::CapabilityCatalog;
use crate::channel::{CapabilityChannel, StepOutput};
use crate::runtime::ControllerRuntime;

/// Bootstrap the Webots-owned controller.
///
/// The controller joins the supervised run through the same strict Clap argv
/// as every participant. Its producer identity is the bus session it opens,
/// and it always mints its own timeline: a world history belongs to the
/// controller process that runs it, never to the CLI.
pub(crate) fn run() -> Result<()> {
    phoxal::run::<WebotsControllerSimulator>()
}

/// The one contract this controller publishes as itself rather than on behalf
/// of a device. Every capability's own handle lives with the device serving it.
#[derive(Clone)]
pub(crate) struct Api {
    clock: WorldClockPublisher<ClockEndpoint>,
}

impl Api {
    /// Publish everything one completed world advance produced, then the clock
    /// that closes it. The order is the contract: a reader that has seen the
    /// clock for a step has already seen that step's outputs.
    pub(crate) fn commit_step(
        &self,
        world_step: &WorldStepToken,
        step: u64,
        output: StepOutput,
    ) -> Result<()> {
        output.publish(world_step)?;
        self.clock.publish(world_step, Clock { step })?;
        Ok(())
    }
}

pub(crate) struct WebotsControllerState {
    backend: SharedBackend,
}

#[phoxal::simulator(state = WebotsControllerState, api = Api)]
pub(crate) struct WebotsControllerSimulator;

impl Participant for WebotsControllerSimulator {
    async fn setup(
        &self,
        ctx: &mut SetupContext<Self>,
        _config: Self::Config,
    ) -> Result<(Self::State, Self::Api)> {
        // Open Webots before validating the catalog: the world's actual
        // basicTimeStep determines both device-period quantization and the
        // effective source cadence schedules are allowed to publish at.
        let handle = WebotsHandle::open()?;
        let catalog = CapabilityCatalog::from_robot(ctx.robot()?, handle.basic_time_step_ms())?;
        let clock = ctx.world_clock_publisher()?;

        // One pass over the catalog binds each capability's device and its bus
        // handle together, so the two are never matched up by position later.
        let mut channels = Vec::with_capacity(catalog.specs().len());
        for spec in catalog.specs() {
            channels.push(CapabilityChannel::bind(ctx, handle.webots(), spec).await?);
        }
        tracing::info!(
            target: "simulator_webots_controller",
            capabilities = ?catalog.kind_counts(),
            "webots controller simulator ready"
        );

        let backend = SharedBackend::new(handle.into_backend(channels));
        let api = Api { clock };
        let runtime =
            ControllerRuntime::new(ctx.timeline_authority(TimelineId::mint())?, backend.clone());
        let loop_api = api.clone();
        ctx.spawn_managed(
            "webots-step-loop",
            async move { runtime.run(loop_api).await },
        );

        Ok((WebotsControllerState { backend }, api))
    }

    async fn shutdown(&self, _api: &Self::Api, state: &mut Self::State) -> Result<()> {
        state.backend.park().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::channel::PendingPublish;
    use phoxal::__private::ParticipantSpec;
    use phoxal::Participant;
    use phoxal::api;
    use phoxal_bus::{
        BusConfig, BusOwner, SamplePublisher, SampleReceiver, StreamReceiver, TimelineAuthority,
    };
    use std::time::Duration;

    #[test]
    fn identity_and_kind_are_reported() {
        assert_eq!(
            <WebotsControllerSimulator as ParticipantSpec>::ID,
            "webots-controller"
        );
        assert_eq!(
            <WebotsControllerSimulator as ParticipantSpec>::KIND,
            phoxal::__private::ParticipantKind::Simulator
        );
        assert!(
            <WebotsControllerSimulator as Participant>::__step_schedule().is_none(),
            "the controller must not wrap Webots in a framework step loop"
        );
    }

    // One process may mint exactly one timeline authority, so the commit
    // path's behaviour is covered by this single test rather than one per
    // assertion.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn a_step_publishes_its_outputs_before_its_clock() {
        let (owner, bus) = BusOwner::open(BusConfig::for_participant(
            phoxal_bus::ExecutionId::mint(),
            phoxal_bus::ParticipantId::new("webots-controller").expect("valid participant id"),
            Vec::new(),
        ))
        .await
        .expect("bus should open");
        let clock_subscriber = StreamReceiver::<ClockEndpoint>::new(
            &bus,
            &phoxal_protocol::runtime::topic::client()
                .simulation()
                .clock(),
        )
        .await
        .expect("clock subscriber should attach");
        let encoder_subscriber =
            SampleReceiver::<api::endpoint::component::encoder::SampleEndpoint>::new(
                &bus,
                &api::topic::client()
                    .component("left_drive")
                    .expect("valid component segment")
                    .encoder("encoder")
                    .expect("valid capability segment")
                    .sample(),
            )
            .await
            .expect("encoder subscriber should attach");
        let encoder_publisher = SamplePublisher::new(
            bus.clone(),
            &api::topic::owner()
                .component("left_drive")
                .expect("valid component segment")
                .encoder("encoder")
                .expect("valid capability segment")
                .sample(),
        )
        .expect("encoder publisher should attach");
        let api = Api {
            clock: WorldClockPublisher::mint(
                bus.clone(),
                &phoxal_protocol::runtime::topic::owner()
                    .simulation()
                    .clock(),
            )
            .expect("clock publisher should attach"),
        };

        let timeline = TimelineId::from_raw(77).expect("test timeline must be nonzero");
        let authority = TimelineAuthority::mint(timeline).expect("authority should mint");
        let world_step = authority.completed_step(20_000_000);
        let output = StepOutput::new(vec![PendingPublish::Encoder(
            encoder_publisher,
            api::component::encoder::Sample::try_new(1.0, 0.5).unwrap(),
        )]);
        api.commit_step(&world_step, 2, output)
            .expect("commit should publish");

        let encoder = tokio::time::timeout(Duration::from_secs(2), encoder_subscriber.recv())
            .await
            .expect("encoder output should arrive")
            .expect("encoder output should decode");
        let clock = tokio::time::timeout(Duration::from_secs(2), clock_subscriber.recv())
            .await
            .expect("clock should arrive")
            .expect("clock should decode");

        // Every output of one completed world step shares that step's exact
        // instant, and it rides in the envelope rather than in any body.
        let expected = RobotInstant::new(timeline, 20_000_000);
        assert_eq!(encoder.metadata.produced_exactly_at(), Some(expected));
        assert_eq!(clock.metadata.produced_exactly_at(), Some(expected));
        assert_eq!(clock.body.step, 2);
        assert!(
            encoder.metadata.sequence < clock.metadata.sequence,
            "all completed-world outputs must enqueue before the matching clock"
        );
        owner.close().await;
    }
}
