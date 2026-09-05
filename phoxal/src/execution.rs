//! The one private attachment sequence for a running execution.
//!
//! This module is deliberately outside the optional public `session` surface.
//! Participants, external sessions, and simulator hosts all need the same
//! bootstrap, while their owned transport lifetimes and role-specific work
//! remain separate.

use crate::bus::{BusError, BusHandle, DEFAULT_QUERY_TIMEOUT, Querier, QueryError, StreamReceiver};
use crate::identity::ExecutionId;
use crate::supervisor::api;
use crate::supervisor::api::connect::{ConnectReply, ConnectRequest};
use crate::supervisor::api::simulation::SimulationAttachmentState;
use crate::supervisor::api::time_domain::TimeDomain;
use crate::version::FrameworkVersion;

/// Facts obtained during the one framework-owned attachment bootstrap.
///
/// The stream is subscribed before `current` is asked, then every buffered
/// replacement is reconciled before this primitive returns. Role-specific
/// startup owns the stream after that gap-free initial snapshot.
pub(crate) struct ExecutionBootstrap {
    #[allow(
        dead_code,
        reason = "the optional session surface retains this immutable attachment fact"
    )]
    pub(crate) execution: ExecutionId,
    #[allow(
        dead_code,
        reason = "the optional session surface retains this immutable attachment fact"
    )]
    pub(crate) framework: FrameworkVersion,
    pub(crate) info: api::info::InfoResponse,
    pub(crate) time_domain: TimeDomain,
    pub(crate) time_domains: StreamReceiver<api::time_domain::TimeDomainStream>,
    pub(crate) attachment: Option<SimulationAttachmentState>,
    pub(crate) attachments: StreamReceiver<api::simulation::attachment::SimulationAttachmentStream>,
}

/// A failure while attaching to a supervisor before role-specific startup.
#[derive(Debug, thiserror::Error)]
pub(crate) enum BootstrapError {
    #[error("no Phoxal execution is reachable at {endpoint}")]
    NoExecution { endpoint: String },
    #[error(
        "{count} Phoxal executions are reachable at {endpoint}, which must identify exactly one: {executions:?}"
    )]
    MultipleExecutions {
        endpoint: String,
        count: usize,
        executions: Vec<ExecutionId>,
    },
    #[error("remote framework {remote} is incompatible with local framework {local}: {refusal}")]
    IncompatibleFramework {
        remote: FrameworkVersion,
        local: FrameworkVersion,
        refusal: CompatibilityRefusal,
    },
    #[error("the frozen supervisor bootstrap reply could not be decoded: {detail}")]
    UnreadableBootstrap { detail: String },
    #[error(transparent)]
    Bus(#[from] BusError),
    #[error(transparent)]
    Query(#[from] QueryError),
}

/// Which compatible peer is on the newer framework line.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CompatibilityRefusal {
    RemoteNewer,
    LocalNewer,
}

impl std::fmt::Display for CompatibilityRefusal {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::RemoteNewer => formatter.write_str("remote framework line is newer"),
            Self::LocalNewer => formatter.write_str("local framework line is newer"),
        }
    }
}

/// Resolve the execution identity the rendezvous endpoint currently exposes.
pub(crate) async fn resolve_execution(endpoint: &str) -> Result<ExecutionId, BootstrapError> {
    let executions = crate::bus::BusOwner::probe_routers(endpoint).await?;
    exactly_one_execution(endpoint, executions)
}

/// Enforce the one-execution rendezvous rule with stable diagnostics.
fn exactly_one_execution(
    endpoint: &str,
    mut executions: Vec<ExecutionId>,
) -> Result<ExecutionId, BootstrapError> {
    match executions.as_slice() {
        [execution] => Ok(*execution),
        [] => Err(BootstrapError::NoExecution {
            endpoint: endpoint.to_owned(),
        }),
        _ => {
            executions.sort_by_key(ToString::to_string);
            Err(BootstrapError::MultipleExecutions {
                endpoint: endpoint.to_owned(),
                count: executions.len(),
                executions,
            })
        }
    }
}

/// Complete the frozen supervisor bootstrap on a caller-owned bus session.
pub(crate) async fn attach_execution(
    bus: &BusHandle,
) -> Result<ExecutionBootstrap, BootstrapError> {
    let framework = remote_framework(bus).await?;
    ensure_compatible_framework(framework, FrameworkVersion::CURRENT)?;

    let info = Querier::new(
        bus.clone(),
        &api::topics().info().client(),
        DEFAULT_QUERY_TIMEOUT,
    )?
    .query(api::info::InfoRequest {})
    .await?;

    let time_domains = StreamReceiver::new(bus, &api::topics().time_domain().client()).await?;
    let current = Querier::new(
        bus.clone(),
        &api::topics().time_domain().current().client(),
        DEFAULT_QUERY_TIMEOUT,
    )?
    .query(api::time_domain::CurrentRequest {})
    .await?;
    let mut time_domain = current.domain;
    reconcile_time_domain(&mut time_domain, &time_domains)?;

    let attachments =
        StreamReceiver::new(bus, &api::topics().simulation().attachment().client()).await?;
    let current_attachment = Querier::new(
        bus.clone(),
        &api::topics().simulation().attachment().current().client(),
        DEFAULT_QUERY_TIMEOUT,
    )?
    .query(api::simulation::attachment::CurrentRequest {})
    .await?;
    let mut attachment = current_attachment.attachment;
    reconcile_attachment(&mut attachment, &attachments)?;

    Ok(ExecutionBootstrap {
        execution: bus.execution(),
        framework,
        info,
        time_domain,
        time_domains,
        attachment,
        attachments,
    })
}

fn reconcile_attachment(
    current: &mut Option<SimulationAttachmentState>,
    updates: &StreamReceiver<api::simulation::attachment::SimulationAttachmentStream>,
) -> Result<(), BootstrapError> {
    while let Some(update) = updates.try_recv()? {
        let replacement = update.body.attachment;
        match (replacement, *current) {
            (Some(replacement), Some(installed)) if replacement.revision > installed.revision => {
                *current = Some(replacement);
            }
            (Some(replacement), None) => *current = Some(replacement),
            // The stream's initial `None` may still be buffered after a newer
            // current query returned an attachment. It carries no revision and
            // must never erase that source-bound state.
            (None, _) | (Some(_), Some(_)) => {}
        }
    }
    Ok(())
}

/// Install every already-buffered replacement that is newer than `current`.
///
/// Subscribing before the query closes the transport race, but a replacement
/// can still arrive after the query response was produced. Draining without an
/// await makes the bootstrap return the newest known domain while preserving
/// later arrivals for the role-specific lifecycle.
fn reconcile_time_domain(
    current: &mut TimeDomain,
    updates: &StreamReceiver<api::time_domain::TimeDomainStream>,
) -> Result<(), BootstrapError> {
    while let Some(update) = updates.try_recv()? {
        if update.body.domain.revision > current.revision {
            *current = update.body.domain;
        }
    }
    Ok(())
}

async fn remote_framework(bus: &BusHandle) -> Result<FrameworkVersion, BootstrapError> {
    let reply = Querier::new(
        bus.clone(),
        &api::topics().connect().client(),
        DEFAULT_QUERY_TIMEOUT,
    )?
    .query(ConnectRequest::V0 {})
    .await
    .map_err(|error| match error {
        QueryError::Decode(detail) => BootstrapError::UnreadableBootstrap { detail },
        other => BootstrapError::Query(other),
    })?;
    let ConnectReply::V0 { framework } = reply;
    Ok(framework)
}

pub(crate) fn ensure_compatible_framework(
    remote: FrameworkVersion,
    local: FrameworkVersion,
) -> Result<(), BootstrapError> {
    if remote.is_compatible_with(local) {
        return Ok(());
    }
    let refusal = if version_key(remote) > version_key(local) {
        CompatibilityRefusal::RemoteNewer
    } else {
        CompatibilityRefusal::LocalNewer
    };
    Err(BootstrapError::IncompatibleFramework {
        remote,
        local,
        refusal,
    })
}

const fn version_key(version: FrameworkVersion) -> (u16, u16, u16) {
    (version.major(), version.minor(), version.patch())
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::{BootstrapError, attach_execution, exactly_one_execution};
    use crate::bus::{BusConfig, BusOwner, Codec, MessagePack, StreamPublisher, StreamReceiver};
    use crate::identity::{ExecutionId, ParticipantId, TimelineId};
    use crate::model::builder::RobotBuilder;
    use crate::model::manifest::ManifestDocument;
    use crate::supervisor::api;
    use crate::supervisor::api::connect::{ConnectReply, ConnectRequest};
    use crate::supervisor::api::time_domain::{TimeDomain, TimeDomainStream, TimeMode};
    use crate::version::FrameworkVersion;

    fn domain(revision: u64, timeline: u64, mode: TimeMode) -> TimeDomain {
        TimeDomain {
            revision,
            timeline: TimelineId::from_raw(timeline).expect("a nonzero test timeline"),
            mode,
        }
    }

    #[test]
    fn ambiguous_execution_diagnostics_are_deterministic() {
        let lower = ExecutionId::parse("10000000000000000000000000000001")
            .expect("a canonical execution id");
        let higher = ExecutionId::parse("20000000000000000000000000000002")
            .expect("a canonical execution id");
        let error = exactly_one_execution("tcp/router:7447", vec![higher, lower])
            .expect_err("two executions are ambiguous");
        let BootstrapError::MultipleExecutions {
            count, executions, ..
        } = error
        else {
            panic!("the ambiguity must retain both execution identities");
        };
        assert_eq!(count, 2);
        assert_eq!(executions, vec![lower, higher]);
    }

    /// The attachment stream subscribes before `current`, so replacements that
    /// happen during that query are reconciled without consuming later updates.
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn attachment_reconciles_buffered_time_domains_without_a_receive_gap() {
        let execution = ExecutionId::mint();
        let participant = ParticipantId::new("bootstrap-domain").expect("valid participant id");
        let (owner, bus) = BusOwner::open(BusConfig::for_participant(
            execution,
            participant,
            Vec::new(),
        ))
        .await
        .expect("the in-process bus opens");
        let connect = bus
            .declare_server(api::topics().connect().owner().key())
            .await
            .expect("the bootstrap server attaches");
        let info = bus
            .declare_server(api::topics().info().owner().key())
            .await
            .expect("the info server attaches");
        let current = bus
            .declare_server(api::topics().time_domain().current().owner().key())
            .await
            .expect("the time-domain server attaches");
        let attachment_current = bus
            .declare_server(
                api::topics()
                    .simulation()
                    .attachment()
                    .current()
                    .owner()
                    .key(),
            )
            .await
            .expect("the attachment current server attaches");
        let _attachment_publisher = StreamPublisher::new(
            bus.clone(),
            &api::topics().simulation().attachment().owner(),
        )
        .expect("the attachment publisher attaches");
        let publisher = StreamPublisher::new(bus.clone(), &api::topics().time_domain().owner())
            .expect("the time-domain publisher attaches");
        let delivery =
            StreamReceiver::<TimeDomainStream>::new(&bus, &api::topics().time_domain().client())
                .await
                .expect("the delivery observer subscribes");

        let initial = domain(10, 1, TimeMode::Monotonic);
        let stale = domain(9, 2, TimeMode::Simulated);
        let first = domain(11, 3, TimeMode::Simulated);
        let duplicate = domain(11, 4, TimeMode::Monotonic);
        let second = domain(12, 5, TimeMode::Monotonic);
        let buffered = [stale, first, duplicate, second];
        let manifest = ManifestDocument::new(
            RobotBuilder::new("rover")
                .build()
                .expect("a minimal robot is valid"),
        );
        let server_bus = bus.clone();
        let server_publisher = publisher.clone();
        let serving = tokio::spawn(async move {
            let incoming = connect.recv().await?;
            assert_eq!(
                MessagePack::decode::<ConnectRequest>(&incoming.request_bytes()?)?,
                ConnectRequest::V0 {}
            );
            incoming
                .reply(
                    &server_bus,
                    MessagePack::encode(&ConnectReply::V0 {
                        framework: FrameworkVersion::CURRENT,
                    })?,
                )
                .await?;

            let incoming = info.recv().await?;
            let _: api::info::InfoRequest = MessagePack::decode(&incoming.request_bytes()?)?;
            incoming
                .reply(
                    &server_bus,
                    MessagePack::encode(&api::info::InfoResponse { manifest })?,
                )
                .await?;

            let incoming = current.recv().await?;
            let _: api::time_domain::CurrentRequest =
                MessagePack::decode(&incoming.request_bytes()?)?;
            for domain in buffered {
                server_publisher.send(TimeDomainStream { domain })?;
            }
            for expected in buffered {
                let delivered = tokio::time::timeout(Duration::from_secs(2), delivery.recv())
                    .await
                    .expect("each buffered replacement reaches the observer")?;
                assert_eq!(delivered.body.domain, expected);
            }
            incoming
                .reply(
                    &server_bus,
                    MessagePack::encode(&api::time_domain::CurrentResponse { domain: initial })?,
                )
                .await?;

            let incoming = attachment_current.recv().await?;
            let _: api::simulation::attachment::CurrentRequest =
                MessagePack::decode(&incoming.request_bytes()?)?;
            incoming
                .reply(
                    &server_bus,
                    MessagePack::encode(&api::simulation::attachment::CurrentResponse {
                        attachment: None,
                    })?,
                )
                .await?;
            Ok::<(), anyhow::Error>(())
        });

        let bootstrap = attach_execution(&bus)
            .await
            .expect("the attachment bootstrap succeeds");
        serving
            .await
            .expect("the bootstrap server does not panic")
            .expect("the bootstrap server succeeds");
        assert_eq!(bootstrap.execution, bus.execution());
        assert_eq!(bootstrap.time_domain, second);

        let later = domain(13, 6, TimeMode::Simulated);
        publisher
            .send(TimeDomainStream { domain: later })
            .expect("a later replacement is admitted");
        let observed = tokio::time::timeout(Duration::from_secs(2), bootstrap.time_domains.recv())
            .await
            .expect("later replacements remain for the caller")
            .expect("the later replacement decodes");
        assert_eq!(observed.body.domain, later);

        drop(bootstrap);
        drop(publisher);
        let _ = owner.close().await;
    }
}
