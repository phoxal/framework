use super::*;

/// The frozen attachment bootstrap.
///
/// It answers with this supervisor's framework train and nothing else, and it is
/// declared alongside every other endpoint so a client that disagrees learns
/// that from the first thing it asks rather than from a decode failure. The
/// robot this supervisor runs is not here: a client asks `supervisor/info` for
/// it once the two trains have agreed, which keeps this document exactly what
/// every framework line can decode.
pub(super) async fn serve_connect(bus: BusHandle) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().connect().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let ConnectRequest::V0 {} = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(&incoming, &bus, &connect_reply()).await?;
    }
}
pub(super) fn connect_reply() -> ConnectReply {
    ConnectReply::V0 {
        framework: FrameworkVersion::CURRENT,
    }
}

/// Which robot this supervisor is running.
///
/// The answer is the manifest document the supervisor opened, so a client
/// reads exactly what every participant of this execution reads instead of a
/// projection that could disagree with it. The supervisor holds one bundle for
/// the life of the process, so the reply never changes.
pub(super) async fn serve_info(bus: BusHandle, manifest: ManifestDocument) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().info().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let supervisor::info::InfoRequest {} = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        reply(
            &incoming,
            &bus,
            &supervisor::info::InfoResponse {
                manifest: manifest.clone(),
            },
        )
        .await?;
    }
}
