use super::*;

pub(super) fn terminal_outcome(
    live_error: Option<String>,
    failure_reason: Option<SimulationEndReason>,
    cleanup_detail: Option<String>,
) -> TerminalOutcome {
    match (live_error, failure_reason, cleanup_detail) {
        (None, None, None) => TerminalOutcome::Stopped {
            reason: SimulationEndReason::WorldStopped,
        },
        (live_error, Some(reason), cleanup_detail) => TerminalOutcome::Failed {
            reason,
            detail: [live_error, cleanup_detail]
                .into_iter()
                .flatten()
                .reduce(|left, right| format!("{left}; {right}"))
                .unwrap_or_else(|| format!("world session failed with {reason:?}")),
        },
        (Some(detail), None, None) => TerminalOutcome::Failed {
            reason: SimulationEndReason::ProtocolViolation,
            detail,
        },
        (live_error, None, Some(cleanup)) => TerminalOutcome::Failed {
            reason: SimulationEndReason::ProtocolViolation,
            detail: live_error.map_or(cleanup.clone(), |live| format!("{live}; {cleanup}")),
        },
    }
}

pub(super) async fn await_world_controller_stop(
    native: &HostServer,
    webots: &mut WebotsProcess,
) -> Result<()> {
    if !native.has_world_controller() {
        return Ok(());
    }
    let deadline = tokio::time::Instant::now() + WORLD_STOP_TIMEOUT;
    loop {
        if native.world_is_stopped() {
            return Ok(());
        }
        if let Some(status) = webots.exited()? {
            bail!("Webots exited with {status} before the world controller acknowledged stop");
        }
        if tokio::time::Instant::now() >= deadline {
            bail!("world controller did not acknowledge stop within {WORLD_STOP_TIMEOUT:?}");
        }
        tokio::time::sleep(RECONCILE_INTERVAL).await;
    }
}

pub(super) fn failing_identity(
    reason: Option<SimulationEndReason>,
    native_lifecycle: &NativeWorldLifecycle,
    members: &[WorldMember],
    native_process: Option<&NativeProcessIdentity>,
) -> TerminalFailure {
    let process = if reason == Some(SimulationEndReason::SimulatorLost) {
        native_process.map(|identity| identity.process)
    } else {
        None
    };
    let native_execution = match native_lifecycle {
        NativeWorldLifecycle::Failed(NativeWorldFailure::RobotControllerLost { execution }) => {
            Some(execution.as_str())
        }
        NativeWorldLifecycle::Starting
        | NativeWorldLifecycle::Ready { .. }
        | NativeWorldLifecycle::Stopping
        | NativeWorldLifecycle::Failed(_) => None,
    };
    let producer = native_execution
        .and_then(|execution| {
            members
                .iter()
                .find(|member| member.execution.to_string() == execution)
                .map(|member| member.controller)
        })
        .or_else(|| match reason {
            Some(SimulationEndReason::MutationFailed) => {
                unique_member_producer(members, WorldMemberPhase::Preparing)
            }
            Some(SimulationEndReason::RemovalFailed) => {
                unique_member_producer(members, WorldMemberPhase::Removing)
            }
            Some(
                SimulationEndReason::WorldStopped
                | SimulationEndReason::HostLost
                | SimulationEndReason::SimulatorLost
                | SimulationEndReason::WorldControllerLost
                | SimulationEndReason::ControllerLost
                | SimulationEndReason::UnsupportedNativeMode
                | SimulationEndReason::InvalidProgress
                | SimulationEndReason::ProtocolViolation,
            )
            | None => None,
        });
    TerminalFailure { process, producer }
}

fn unique_member_producer(
    members: &[WorldMember],
    phase: WorldMemberPhase,
) -> Option<phoxal::identity::ProducerId> {
    let mut candidates = members
        .iter()
        .filter(|member| member.phase == phase)
        .map(|member| member.controller);
    let producer = candidates.next()?;
    candidates.next().is_none().then_some(producer)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::registration::ProcessIdentity;
    use phoxal::bus::RobotInstant;
    use phoxal::identity::{ExecutionId, ProducerId, TimelineId};
    use phoxal::model::identity::{RobotId, SpawnId};
    use phoxal::model::world::{LiveAttachmentBoundary, WorldProgress};

    fn member(execution: &str, producer: u128, phase: WorldMemberPhase) -> WorldMember {
        WorldMember {
            execution: ExecutionId::parse(execution).expect("canonical execution"),
            robot: RobotId::new("robot").expect("robot id"),
            controller: ProducerId::try_from(producer).expect("producer id"),
            phase,
            attached_at: LiveAttachmentBoundary {
                world: WorldProgress::zero(12_000_000).expect("world progress"),
                execution: RobotInstant::new(TimelineId::from_raw(1).expect("timeline"), 0),
            },
            spawn: SpawnId::new("spawn").expect("spawn id"),
            initial_pose: serde_json::from_value(serde_json::json!({
                "xyz": [0.0, 0.0, 0.0],
                "rpy": [0.0, 0.0, 0.0]
            }))
            .expect("pose"),
        }
    }

    #[test]
    fn hard_controller_loss_names_the_exact_pre_cleanup_member() {
        let first = member(
            "10000000000000000000000000000001",
            0x3000_0000_0000_0000_0000_0000_0000_0003,
            WorldMemberPhase::Active,
        );
        let second = member(
            "20000000000000000000000000000002",
            0x4000_0000_0000_0000_0000_0000_0000_0004,
            WorldMemberPhase::Active,
        );
        let lifecycle = NativeWorldLifecycle::Failed(NativeWorldFailure::RobotControllerLost {
            execution: second.execution.to_string(),
        });

        let failing = failing_identity(
            Some(SimulationEndReason::ControllerLost),
            &lifecycle,
            &[first, second.clone()],
            None,
        );

        assert_eq!(failing.producer, Some(second.controller));
        assert_eq!(failing.process, None);
    }

    #[test]
    fn simulator_loss_names_the_owned_native_process() {
        let identity = NativeProcessIdentity {
            process: ProcessIdentity {
                pid: 123,
                started_at_unix_s: 456,
            },
            executable: PathBuf::from("/Applications/Webots.app/Contents/MacOS/webots"),
            process_group: Some(123),
        };

        let failing = failing_identity(
            Some(SimulationEndReason::SimulatorLost),
            &NativeWorldLifecycle::Starting,
            &[],
            Some(&identity),
        );

        assert_eq!(failing.process, Some(identity.process));
        assert_eq!(failing.producer, None);
    }

    #[test]
    fn removal_failure_names_only_an_unambiguous_removing_member() {
        let active = member(
            "10000000000000000000000000000001",
            0x3000_0000_0000_0000_0000_0000_0000_0003,
            WorldMemberPhase::Active,
        );
        let removing = member(
            "20000000000000000000000000000002",
            0x4000_0000_0000_0000_0000_0000_0000_0004,
            WorldMemberPhase::Removing,
        );

        let failing = failing_identity(
            Some(SimulationEndReason::RemovalFailed),
            &NativeWorldLifecycle::Starting,
            &[active, removing.clone()],
            None,
        );

        assert_eq!(failing.producer, Some(removing.controller));
    }

    #[test]
    fn terminal_cleanup_failure_cannot_be_reported_as_an_orderly_stop() {
        let outcome = terminal_outcome(
            None,
            Some(SimulationEndReason::RemovalFailed),
            Some("Robot controller did not confirm parked".to_owned()),
        );
        assert!(matches!(
            outcome,
            TerminalOutcome::Failed {
                reason: SimulationEndReason::RemovalFailed,
                ref detail,
            } if detail.contains("did not confirm parked")
        ));
    }
}
