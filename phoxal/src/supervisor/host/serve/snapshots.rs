use super::*;

pub(super) async fn serve_snapshots(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let publisher = StreamPublisher::new(bus, &supervisor::topics().snapshot().owner())?;
    let mut snapshots = state.subscribe();
    publisher.send(SnapshotDocument::V0(snapshots.borrow_and_update().clone()))?;
    loop {
        snapshots
            .changed()
            .await
            .context("the supervisor snapshot authority closed")?;
        publisher.send(SnapshotDocument::V0(snapshots.borrow_and_update().clone()))?;
    }
}

pub(super) async fn serve_current(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().snapshot().current().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let _: supervisor::snapshot::CurrentRequest = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(&incoming, &bus, &SnapshotDocument::V0(state.snapshot())).await?;
    }
}

/// Publish every complete scheduling-authority replacement in order.
pub(super) async fn serve_time_domains(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let publisher = StreamPublisher::new(bus, &supervisor::topics().time_domain().owner())?;
    let mut domains = state
        .take_time_domain_updates()
        .context("the supervisor time-domain authority is already being served")?;
    while let Some(domain) = domains.recv().await {
        publisher.send(supervisor::time_domain::TimeDomainStream { domain })?;
    }
    bail!("the supervisor time-domain authority closed")
}

/// Answer the current domain after a client subscribed to its replacement
/// stream, closing the ordinary subscribe/query race.
pub(super) async fn serve_current_time_domain(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().time_domain().current().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let _: supervisor::time_domain::CurrentRequest = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(
            &incoming,
            &bus,
            &supervisor::time_domain::CurrentResponse {
                domain: state.time_domain(),
            },
        )
        .await?;
    }
}
