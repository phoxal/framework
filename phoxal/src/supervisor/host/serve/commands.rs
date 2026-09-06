use super::*;

pub(super) async fn serve_commands(bus: BusHandle, state: ExecutionState) -> Result<()> {
    let server = declare(&bus, &supervisor::topics().command().owner()).await?;
    loop {
        let incoming = server.recv().await?;
        let request: supervisor::command::Request = match decode(&incoming).await? {
            Some(request) => request,
            None => continue,
        };
        let supervisor::command::Request::V0 { command: request } = request;
        let (outcome, action) = command(&state, request);
        // Acceptance reaches the client before the host is asked to go down;
        // reversing these turns an accepted reboot into an ambiguous
        // no-responder failure at the caller.
        reply(&incoming, &bus, &supervisor::command::Reply::V0 { outcome }).await?;
        action.request().await;
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum HostAction {
    Reboot,
    Poweroff,
}

impl HostAction {
    async fn request(self) {
        let name = match self {
            Self::Reboot => "reboot",
            Self::Poweroff => "power-off",
        };
        let result = tokio::task::spawn_blocking(move || match self {
            Self::Reboot => system_shutdown::reboot(),
            Self::Poweroff => system_shutdown::shutdown(),
        })
        .await;
        match result {
            Ok(Ok(())) => {}
            Ok(Err(error)) => tracing::error!(action = name, %error, "host action failed"),
            Err(error) => tracing::error!(action = name, %error, "host action task failed"),
        }
    }
}

/// Accept one host request, and say which execution revision it was accepted
/// at.
///
/// The revision is evidence, not a fence: whether cycling this machine's power
/// is safe is the operator's judgment about the machine, and how many times a
/// Ready lease has moved since they last looked says nothing about it.
pub(super) fn command(state: &ExecutionState, command: Command) -> (CommandOutcome, HostAction) {
    let action = match command {
        Command::Reboot => HostAction::Reboot,
        Command::Poweroff => HostAction::Poweroff,
    };
    (
        CommandOutcome::Accepted {
            at_revision: state.snapshot().revision,
        },
        action,
    )
}
