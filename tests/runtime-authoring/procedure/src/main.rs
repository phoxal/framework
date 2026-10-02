//! The component-owned bounded procedure fixture.
//!
//! A component-shaped runtime — a leased actuator setpoint projection and
//! a queued stage log, no brain and no remote calls — owns one local
//! procedure tree of bounded action stages anchored at the accepting
//! command's instant. The fixture proves the behavior facility is ordinary
//! component authoring too: stage timing follows the runtime clock, the
//! projection leases only the current stage's velocity, no device
//! calibration is invented, and the procedure cancels cleanly on command.

use phoxal::Result;
use phoxal::contracts::component::actuator::{ActuatorSetpoint, ActuatorTarget, Control};
use phoxal::runtime::Context;
use phoxal::runtime::behavior::{ActionOutcome, Node, Tree, TreeStatus, action, sequence};

#[phoxal::messages(package = "phoxal.tests.authoring.procedure.v1")]
mod v1 {
    use phoxal::contracts::component::actuator::ActuatorSetpoint;
    use phoxal::contracts::{Latest, Queue, RequestReply};

    /// A command to run one bounded procedure.
    pub struct RunRequest {
        #[phoxal(tag = 1)]
        pub sequence_id: u64,
    }

    pub enum RunResponse {
        #[phoxal(tag = 1)]
        Accepted,
        #[phoxal(tag = 2)]
        Busy,
    }

    pub struct HaltRequest {
        #[phoxal(tag = 1)]
        pub sequence_id: u64,
    }

    pub enum HaltResponse {
        #[phoxal(tag = 1)]
        Halted,
        #[phoxal(tag = 2)]
        NotRunning,
    }

    /// One recorded procedure stage transition.
    pub struct StageLog {
        #[phoxal(tag = 1)]
        pub sequence_id: u64,
        #[phoxal(tag = 2)]
        pub stage: u32,
    }

    /// The fixture's endpoint contract: a leased setpoint projection, the
    /// queued stage log, and the procedure's command surface.
    #[phoxal::endpoints]
    pub struct ProcedureApi {
        #[phoxal::output(projection = state, lease_ms = 100, max_bytes = 1_024)]
        setpoint: Latest<ActuatorSetpoint>,

        #[phoxal::output(max_items = 64, max_bytes = 4_096)]
        stages: Queue<StageLog>,

        #[phoxal::operation(max_items = 8, max_bytes = 1_024)]
        run: RequestReply<RunRequest, RunResponse>,

        #[phoxal::operation(max_items = 8, max_bytes = 1_024)]
        halt: RequestReply<HaltRequest, HaltResponse>,
    }
}

pub use v1::*;

/// The procedure's commanded velocity per stage: a gentle ramp with a
/// bounded hold, then a commanded stop. Stage boundaries are fixed by the
/// procedure definition on the runtime clock, never by device feedback.
const STAGE_VELOCITIES_RADPS: [f64; 4] = [0.0, 0.2, 0.2, 0.0];
const STAGE_BOUNDARY_MS: u64 = 40;

struct ProcedureDriver {
    sequence_id: Option<u64>,
    started_at_ms: Option<u64>,
    stage: u32,
    procedure: Option<Tree<ProcedureDriver>>,
}

#[phoxal::runtime(contract = ProcedureApi, period_ms = 10)]
impl ProcedureDriver {
    #[init]
    fn new(_config: ()) -> Result<Self> {
        Ok(Self {
            sequence_id: None,
            started_at_ms: None,
            stage: 0,
            procedure: None,
        })
    }

    #[handle(run)]
    fn run(&mut self, ctx: &mut Context<'_, Self>, request: RunRequest) -> Result<RunResponse> {
        if self
            .procedure
            .as_ref()
            .is_some_and(|p| p.status() == TreeStatus::Running)
        {
            return Ok(RunResponse::Busy);
        }
        let start_ms = ctx.now().as_nanos() / 1_000_000;
        self.sequence_id = Some(request.sequence_id);
        self.started_at_ms = Some(start_ms);
        self.stage = 0;
        self.procedure = Some(Self::procedure(start_ms, request.sequence_id)?);
        ctx.emit_stages(StageLog {
            sequence_id: request.sequence_id,
            stage: 0,
        })?;
        Ok(RunResponse::Accepted)
    }

    #[handle(halt)]
    fn halt(&mut self, ctx: &mut Context<'_, Self>, request: HaltRequest) -> Result<HaltResponse> {
        let Some(procedure) = self.procedure.as_mut() else {
            return Ok(HaltResponse::NotRunning);
        };
        if procedure.status() != TreeStatus::Running
            || self.sequence_id != Some(request.sequence_id)
        {
            return Ok(HaltResponse::NotRunning);
        }
        procedure.cancel(ctx)?;
        self.stage = stop_stage();
        ctx.emit_stages(StageLog {
            sequence_id: request.sequence_id,
            stage: self.stage,
        })?;
        Ok(HaltResponse::Halted)
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> Result<()> {
        if let Some(procedure) = self.procedure.as_mut() {
            procedure.tick(ctx)?;
        }
        // The stage projection follows the same runtime clock the tree's
        // action stages anchor to, so the leased velocity always matches
        // the stage the tree has entered.
        if self
            .procedure
            .as_ref()
            .is_some_and(|p| p.status() == TreeStatus::Running)
            && let Some(started) = self.started_at_ms
            && let Some(sequence_id) = self.sequence_id
        {
            let now_ms = ctx.now().as_nanos() / 1_000_000;
            let elapsed = now_ms.saturating_sub(started);
            let stage = u32::try_from(elapsed / STAGE_BOUNDARY_MS)
                .unwrap_or(u32::MAX)
                .min(stop_stage());
            // The tree's action stages own the stage log; this derived
            // value only keeps the projection's leased velocity aligned
            // with the stage the tree has entered.
            let _ = sequence_id;
            self.stage = stage;
        }
        Ok(())
    }

    /// Projects the current stage's leased velocity command. A stopped,
    /// halted, or absent procedure always commands zero velocity.
    #[publish(setpoint)]
    fn setpoint(&self) -> Option<ActuatorSetpoint> {
        let running = self
            .procedure
            .as_ref()
            .is_some_and(|p| p.status() == TreeStatus::Running);
        let index = if running {
            usize::try_from(self.stage).unwrap_or(0)
        } else {
            STAGE_VELOCITIES_RADPS.len() - 1
        }
        .min(STAGE_VELOCITIES_RADPS.len() - 1);
        Some(ActuatorSetpoint {
            targets: vec![ActuatorTarget {
                actuator_id: "procedure_joint".to_owned(),
                control: Some(Control::VelocityRadps(STAGE_VELOCITIES_RADPS[index])),
            }],
        })
    }

    /// The bounded local procedure: one action stage per stage boundary,
    /// each anchored at an absolute deadline computed from the accepting
    /// command's instant. No remote calls and no device calibration.
    fn procedure(start_ms: u64, sequence_id: u64) -> Result<Tree<ProcedureDriver>> {
        sequence([
            Self::stage_action(start_ms + STAGE_BOUNDARY_MS, sequence_id, 1),
            Self::stage_action(start_ms + 2 * STAGE_BOUNDARY_MS, sequence_id, 2),
            Self::stage_action(start_ms + 3 * STAGE_BOUNDARY_MS, sequence_id, 3),
        ])
        .build()
    }

    /// One bounded action stage: it runs until its absolute deadline, then
    /// logs the stage it completed and succeeds exactly once.
    fn stage_action(deadline_ms: u64, sequence_id: u64, stage: u32) -> Node<ProcedureDriver> {
        action(move |ctx: &mut Context<'_, Self>| {
            let now_ms = ctx.now().as_nanos() / 1_000_000;
            if now_ms < deadline_ms {
                return Ok(ActionOutcome::Running);
            }
            ctx.emit_stages(StageLog { sequence_id, stage })?;
            Ok(ActionOutcome::Succeeded)
        })
        .into_node()
    }
}

/// The terminal, zero-velocity stage index.
fn stop_stage() -> u32 {
    (STAGE_VELOCITIES_RADPS.len() - 1) as u32
}

fn main() -> Result<()> {
    phoxal::runtime::run::<ProcedureDriver>()
}

/// Deterministic owner-path proof: the component-owned procedure advances
/// one stage per boundary, leases the matching velocity, refuses
/// concurrent runs, and halts on command.
#[cfg(test)]
mod tests {
    use super::ProcedureDriver;
    use super::procedure_api;
    use super::{HaltRequest, HaltResponse, RunRequest, RunResponse};
    use phoxal::runtime::input::{CommandOrder, Commands, InputSnapshot};
    use phoxal::runtime::{
        CommandId, ExecutionDuration, ExecutionTime, StepContext, initialize, invoke,
    };

    struct Fixture {
        adapter: super::phoxal_runtime_procedure_driver::Adapter,
        service: Option<ProcedureDriver>,
    }

    impl Fixture {
        fn new() -> Self {
            let adapter = super::phoxal_runtime_procedure_driver::Adapter::new();
            let service =
                initialize(&adapter, ExecutionTime::default(), ()).expect("fixture initializes");
            Self {
                adapter,
                service: Some(service),
            }
        }

        fn step(
            &mut self,
            index: u64,
            millis: u64,
            inputs: &procedure_api::Inputs,
        ) -> phoxal::Result<procedure_api::Outputs> {
            let context = StepContext::from_previous(
                ExecutionTime::from_nanos(millis * 1_000_000),
                ExecutionDuration::from_millis(10),
                Some(ExecutionTime::from_nanos(
                    millis.saturating_sub(10) * 1_000_000,
                )),
                0,
                index,
            );
            let service = self.service.take().expect("the service exists");
            let (service, outputs) = invoke(&self.adapter, &context, service, inputs)?;
            self.service = Some(service);
            Ok(outputs)
        }

        fn stage_velocity(&mut self) -> f64 {
            self.service
                .as_ref()
                .and_then(|service| service.setpoint())
                .and_then(|setpoint| setpoint.targets.first().cloned())
                .and_then(|target| match target.control {
                    Some(phoxal::contracts::component::actuator::Control::VelocityRadps(v)) => {
                        Some(v)
                    }
                    _ => None,
                })
                .expect("the projection always leases a velocity")
        }
    }

    fn run(sequence_id: u64) -> procedure_api::Inputs {
        let mut inputs = <procedure_api::Inputs as InputSnapshot>::empty();
        inputs.run = Commands::new(vec![phoxal::runtime::Command::with_order(
            CommandOrder::new(1, 0, CommandId::new(1)),
            RunRequest { sequence_id },
        )]);
        inputs
    }

    fn halt(sequence_id: u64) -> procedure_api::Inputs {
        let mut inputs = <procedure_api::Inputs as InputSnapshot>::empty();
        inputs.halt = Commands::new(vec![phoxal::runtime::Command::with_order(
            CommandOrder::new(1, 0, CommandId::new(2)),
            HaltRequest { sequence_id },
        )]);
        inputs
    }

    fn empty() -> procedure_api::Inputs {
        <procedure_api::Inputs as InputSnapshot>::empty()
    }

    #[test]
    fn the_procedure_advances_one_stage_per_boundary() -> phoxal::Result<()> {
        let mut fixture = Fixture::new();
        let accepted = fixture.step(0, 0, &run(4))?;
        assert!(matches!(
            accepted.run_replies[0].response(),
            RunResponse::Accepted
        ));
        assert_eq!(accepted.stages.len(), 1);

        // A concurrent run is refused while the procedure holds the joint.
        let busy = fixture.step(1, 10, &run(5))?;
        assert!(matches!(busy.run_replies[0].response(), RunResponse::Busy));

        // Inside the first boundary the projection still leases stage 0.
        let holding = fixture.step(2, 30, &empty())?;
        assert!(holding.stages.is_empty());
        assert!((fixture.stage_velocity() - 0.0).abs() < 1e-12);

        // The first boundary enters stage 1's ramp; the second holds it.
        let ramped = fixture.step(3, 40, &empty())?;
        assert_eq!(ramped.stages.len(), 1);
        assert_eq!(ramped.stages[0].stage, 1);
        assert!((fixture.stage_velocity() - 0.2).abs() < 1e-12);
        let held = fixture.step(4, 80, &empty())?;
        assert_eq!(held.stages.last().map(|log| log.stage), Some(2));
        assert!((fixture.stage_velocity() - 0.2).abs() < 1e-12);

        // The final boundary commands the stop and the procedure ends.
        let stopped = fixture.step(5, 120, &empty())?;
        assert_eq!(stopped.stages.len(), 1, "the terminal action logged once");
        assert_eq!(stopped.stages[0].stage, 3);
        assert!((fixture.stage_velocity() - 0.0).abs() < 1e-12);

        // A finished procedure accepts a fresh run with a fresh identity.
        let again = fixture.step(6, 130, &run(6))?;
        assert!(matches!(
            again.run_replies[0].response(),
            RunResponse::Accepted
        ));
        assert_eq!(again.stages[0].sequence_id, 6);
        Ok(())
    }

    /// A halt that merges into the same invocation as its run cancels
    /// the procedure before any stage advances: the halt observes the
    /// running mission at its dispatch position and the leased projection
    /// commands the stop velocity immediately.
    #[test]
    fn an_immediate_halt_in_the_run_invocation_stops_before_any_stage() -> phoxal::Result<()> {
        let mut fixture = Fixture::new();
        let mut inputs = run(11);
        inputs.halt = Commands::new(vec![phoxal::runtime::Command::with_order(
            CommandOrder::new(1, 0, CommandId::new(2)),
            HaltRequest { sequence_id: 11 },
        )]);
        let merged = fixture.step(0, 0, &inputs)?;
        assert!(matches!(
            merged.run_replies[0].response(),
            RunResponse::Accepted
        ));
        assert!(matches!(
            merged.halt_replies[0].response(),
            HaltResponse::Halted
        ));
        assert_eq!(merged.stages.len(), 2, "run and terminal stages logged");
        assert_eq!(merged.stages.last().map(|log| log.stage), Some(3));
        assert!((fixture.stage_velocity() - 0.0).abs() < 1e-12);
        Ok(())
    }

    #[test]
    fn halting_cancels_the_procedure_and_leases_the_stop() -> phoxal::Result<()> {
        let mut fixture = Fixture::new();
        fixture.step(0, 0, &run(9))?;
        fixture.step(1, 10, &empty())?;

        let halted = fixture.step(2, 20, &halt(9))?;
        assert!(matches!(
            halted.halt_replies[0].response(),
            HaltResponse::Halted
        ));
        assert_eq!(halted.stages.last().map(|log| log.stage), Some(3));
        assert!((fixture.stage_velocity() - 0.0).abs() < 1e-12);

        // A halted procedure is not running; the same halt is idempotent
        // and a fresh run is accepted with its own identity.
        let repeat = fixture.step(3, 30, &halt(9))?;
        assert!(matches!(
            repeat.halt_replies[0].response(),
            HaltResponse::NotRunning
        ));
        let fresh = fixture.step(4, 40, &run(10))?;
        assert!(matches!(
            fresh.run_replies[0].response(),
            RunResponse::Accepted
        ));
        Ok(())
    }
}
