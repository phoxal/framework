use super::*;

/// A verified client of one local world host.
#[derive(Clone, Debug)]
pub struct WorldSessionClient {
    endpoint: SocketAddr,
    bootstrap: WorldSessionBootstrap,
}

impl WorldSessionClient {
    /// Verify the frozen host bootstrap before trusting the registered endpoint.
    pub async fn connect(endpoint: &str) -> Result<Self, WorldSessionWireError> {
        let endpoint = parse_endpoint(endpoint)?;
        let response: WorldSessionConnectResponse = request(
            endpoint,
            &WorldSessionConnectRequest::Bootstrap {
                framework: FrameworkVersion::CURRENT,
            },
        )
        .await?;
        let WorldSessionConnectResponse::Bootstrap { bootstrap } = response else {
            return Err(WorldSessionWireError::Protocol(
                "world host returned an attachment response to bootstrap".to_owned(),
            ));
        };
        if !bootstrap
            .framework
            .is_compatible_with(FrameworkVersion::CURRENT)
        {
            return Err(WorldSessionWireError::IncompatibleFramework {
                local: FrameworkVersion::CURRENT,
                remote: bootstrap.framework,
            });
        }
        Ok(Self {
            endpoint,
            bootstrap,
        })
    }

    #[must_use]
    pub fn bootstrap(&self) -> &WorldSessionBootstrap {
        &self.bootstrap
    }

    pub async fn current_state(&self) -> Result<WorldSessionState, WorldSessionWireError> {
        let response: WorldSessionStateCurrentResponse = request(
            self.endpoint,
            &WorldSessionStateCurrentRequest {
                instance: self.bootstrap.instance,
            },
        )
        .await?;
        validate_state_against(&self.bootstrap, &response.state)?;
        Ok(response.state)
    }

    pub async fn state_subscription(
        &self,
    ) -> Result<WorldStateSubscription, WorldSessionWireError> {
        let updates = open_subscription(
            self.endpoint,
            &WorldSessionStateSubscriptionRequest {
                instance: self.bootstrap.instance,
            },
        )
        .await?;
        let current = self.current_state().await?;
        WorldStateSubscription::reconcile(self.bootstrap.clone(), current, updates)
    }

    pub async fn current_diagnostics(
        &self,
    ) -> Result<WorldSessionDiagnostics, WorldSessionWireError> {
        let response: WorldSessionDiagnosticsCurrentResponse = request(
            self.endpoint,
            &WorldSessionDiagnosticsCurrentRequest {
                instance: self.bootstrap.instance,
            },
        )
        .await?;
        response.diagnostics.validate()?;
        Ok(response.diagnostics)
    }

    pub async fn diagnostics_subscription(
        &self,
    ) -> Result<WorldDiagnosticsSubscription, WorldSessionWireError> {
        let updates = open_subscription(
            self.endpoint,
            &WorldSessionDiagnosticsSubscriptionRequest {
                instance: self.bootstrap.instance,
            },
        )
        .await?;
        let current = self.current_diagnostics().await?;
        WorldDiagnosticsSubscription::reconcile(current, updates)
    }

    pub async fn control(
        &self,
        operation: WorldControl,
    ) -> Result<WorldSessionState, WorldSessionWireError> {
        let response: WorldSessionControlResponse = request(
            self.endpoint,
            &WorldSessionControlRequest {
                instance: self.bootstrap.instance,
                operation,
            },
        )
        .await?;
        validate_state_against(&self.bootstrap, &response.state)?;
        Ok(response.state)
    }

    pub async fn attach(
        &self,
        execution: ExecutionId,
        supervisor_endpoint: impl Into<String>,
        spawn: Option<SpawnId>,
    ) -> Result<WorldSessionState, WorldSessionWireError> {
        let response: WorldSessionConnectResponse = request(
            self.endpoint,
            &WorldSessionConnectRequest::Attach {
                framework: FrameworkVersion::CURRENT,
                instance: self.bootstrap.instance,
                execution,
                supervisor_endpoint: supervisor_endpoint.into(),
                spawn,
            },
        )
        .await?;
        let WorldSessionConnectResponse::Attached { state } = response else {
            return Err(WorldSessionWireError::Protocol(
                "world host returned a bootstrap response to attachment".to_owned(),
            ));
        };
        validate_state_against(&self.bootstrap, &state)?;
        Ok(state)
    }
}
