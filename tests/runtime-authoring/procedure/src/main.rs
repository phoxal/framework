//! The component-owned bounded procedure fixture.
//!
//! A component-shaped runtime — a leased actuator setpoint projection and
//! a queued stage log, no brain and no remote calls — owns one local
//! procedure tree of bounded action stages anchored at the accepting
//! command's instant. The fixture proves the behavior facility is ordinary
//! component authoring too: stage timing follows the runtime clock, the
//! projection leases only the current stage's velocity, no device
//! calibration is invented, and the procedure cancels cleanly on command.

#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("Phoxal supports Linux and macOS only");

use phoxal::Result;
use phoxal::contracts::component::actuator::{ActuatorCommand, Control};
use phoxal::runtime::Context;
use phoxal::runtime::behavior::{ActionOutcome, Node, Tree, TreeStatus, action, sequence};

#[phoxal::messages(package = "phoxal.tests.authoring.procedure.v1")]
mod v1 {
    use phoxal::contracts::component::actuator::ActuatorCommand;
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
        setpoint: Latest<ActuatorCommand>,

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
    fn setpoint(&self) -> Option<ActuatorCommand> {
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
        Some(ActuatorCommand {
            control: Some(Control::VelocityRadps(STAGE_VELOCITIES_RADPS[index])),
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
    use super::{HaltRequest, HaltResponse, ProcedureDriver, RunRequest, RunResponse};
    use phoxal::runtime::Harness;
    use std::time::Duration;

    fn velocity(harness: &Harness<ProcedureDriver>) -> f64 {
        let setpoint = harness
            .setpoint()
            .expect("the projection leases a setpoint");
        match setpoint
            .targets
            .first()
            .and_then(|target| target.control.as_ref())
        {
            Some(phoxal::contracts::component::actuator::Control::VelocityRadps(value)) => *value,
            _ => panic!("the projection leases a velocity"),
        }
    }

    #[test]
    fn the_procedure_advances_one_stage_per_boundary() -> phoxal::Result<()> {
        let mut harness = Harness::<ProcedureDriver>::new(())?;
        let run = harness.enqueue_run(RunRequest { sequence_id: 4 })?;
        harness.advance_to(Duration::ZERO)?;
        assert!(matches!(harness.reply(run)?, RunResponse::Accepted));
        assert_eq!(harness.stages().len(), 1);

        let busy = harness.enqueue_run(RunRequest { sequence_id: 5 })?;
        harness.advance_to(Duration::from_millis(10))?;
        assert!(matches!(harness.reply(busy)?, RunResponse::Busy));
        harness.advance_to(Duration::from_millis(30))?;
        assert!(harness.stages().is_empty());
        assert!((velocity(&harness) - 0.0).abs() < 1e-12);

        harness.advance_to(Duration::from_millis(40))?;
        let ramped = harness.stages();
        assert_eq!(ramped.len(), 1);
        assert_eq!(ramped[0].stage, 1);
        assert!((velocity(&harness) - 0.2).abs() < 1e-12);
        harness.advance_to(Duration::from_millis(80))?;
        assert_eq!(harness.stages().last().map(|log| log.stage), Some(2));
        assert!((velocity(&harness) - 0.2).abs() < 1e-12);

        harness.advance_to(Duration::from_millis(120))?;
        let stopped = harness.stages();
        assert_eq!(stopped.len(), 1, "the terminal action logged once");
        assert_eq!(stopped[0].stage, 3);
        assert!((velocity(&harness) - 0.0).abs() < 1e-12);
        let again = harness.enqueue_run(RunRequest { sequence_id: 6 })?;
        harness.advance_to(Duration::from_millis(130))?;
        assert!(matches!(harness.reply(again)?, RunResponse::Accepted));
        assert_eq!(harness.stages()[0].sequence_id, 6);
        Ok(())
    }

    #[test]
    fn an_immediate_halt_in_the_run_invocation_stops_before_any_stage() -> phoxal::Result<()> {
        let mut harness = Harness::<ProcedureDriver>::new(())?;
        let run = harness.enqueue_run(RunRequest { sequence_id: 11 })?;
        let halt = harness.enqueue_halt(HaltRequest { sequence_id: 11 })?;
        harness.advance_to(Duration::ZERO)?;
        assert!(matches!(harness.reply(run)?, RunResponse::Accepted));
        assert!(matches!(harness.reply(halt)?, HaltResponse::Halted));
        let stages = harness.stages();
        assert_eq!(stages.len(), 2, "run and terminal stages logged");
        assert_eq!(stages.last().map(|log| log.stage), Some(3));
        assert!((velocity(&harness) - 0.0).abs() < 1e-12);
        Ok(())
    }

    #[test]
    fn halting_cancels_the_procedure_and_leases_the_stop() -> phoxal::Result<()> {
        let mut harness = Harness::<ProcedureDriver>::new(())?;
        let run = harness.enqueue_run(RunRequest { sequence_id: 9 })?;
        harness.advance_to(Duration::from_millis(10))?;
        assert!(matches!(harness.reply(run)?, RunResponse::Accepted));
        harness.stages();
        let halt = harness.enqueue_halt(HaltRequest { sequence_id: 9 })?;
        harness.advance_to(Duration::from_millis(20))?;
        assert!(matches!(harness.reply(halt)?, HaltResponse::Halted));
        assert_eq!(harness.stages().last().map(|log| log.stage), Some(3));
        assert!((velocity(&harness) - 0.0).abs() < 1e-12);

        let repeat = harness.enqueue_halt(HaltRequest { sequence_id: 9 })?;
        harness.advance_to(Duration::from_millis(30))?;
        assert!(matches!(harness.reply(repeat)?, HaltResponse::NotRunning));
        let fresh = harness.enqueue_run(RunRequest { sequence_id: 10 })?;
        harness.advance_to(Duration::from_millis(40))?;
        assert!(matches!(harness.reply(fresh)?, RunResponse::Accepted));
        Ok(())
    }
}
