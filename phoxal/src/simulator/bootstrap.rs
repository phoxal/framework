//! Shared transport bootstrap and frozen facts for Live simulator roles.

use super::*;

/// Framework-owned transport and immutable facts common to every Live role.
///
/// Role-specific sessions retain separate authority after this point. The
/// controller owns device I/O, while the host owns attachment management.
pub(super) struct LiveBootstrap {
    pub(super) owner: BusOwner,
    pub(super) bus: BusHandle,
    pub(super) bootstrap: crate::execution::ExecutionBootstrap,
    pub(super) robot: crate::model::Robot,
    pub(super) assets: crate::bundle::ParticipantAssets,
}

pub(super) async fn open_live_bootstrap(
    connect: String,
    label: String,
) -> Result<LiveBootstrap, SimulatorError> {
    let execution = crate::execution::resolve_execution(&connect)
        .await
        .map_err(simulator_bootstrap_error)?;
    let label = SourceLabel::new(label)?;
    let (owner, bus) = BusOwner::open(BusConfig::for_external(
        execution,
        Some(label),
        vec![connect],
    ))
    .await?;
    let result = async {
        let bootstrap = crate::execution::attach_execution(&bus)
            .await
            .map_err(|error| SimulatorError::Bootstrap {
                detail: error.to_string(),
            })?;
        if bootstrap.time_domain.mode != TimeMode::Monotonic {
            return Err(SimulatorError::NonMonotonicTimeDomain);
        }
        let robot = bootstrap.info.manifest.clone().into_robot();
        let assets =
            crate::bundle::ParticipantAssets::from_supervisor(bus.clone()).map_err(|error| {
                SimulatorError::Bootstrap {
                    detail: error.to_string(),
                }
            })?;
        Ok((bootstrap, robot, assets))
    }
    .await;
    match result {
        Ok((bootstrap, robot, assets)) => Ok(LiveBootstrap {
            owner,
            bus,
            bootstrap,
            robot,
            assets,
        }),
        Err(error) => {
            let _ = owner.close().await;
            Err(error)
        }
    }
}

fn simulator_bootstrap_error(error: crate::execution::BootstrapError) -> SimulatorError {
    match error {
        crate::execution::BootstrapError::NoExecution { endpoint } => {
            SimulatorError::NoExecution { connect: endpoint }
        }
        crate::execution::BootstrapError::MultipleExecutions {
            endpoint,
            count,
            executions,
        } => SimulatorError::MultipleExecutions {
            connect: endpoint,
            count,
            executions,
        },
        error => SimulatorError::Bootstrap {
            detail: error.to_string(),
        },
    }
}
