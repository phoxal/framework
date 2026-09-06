//! Concrete public world-session handler for the Webots adapter.

use std::sync::Arc;

use phoxal::identity::ExecutionId;
use phoxal::model::identity::SpawnId;
use phoxal::world::api::session::connect::WorldSessionBootstrap;
use phoxal::world::api::session::control::WorldControl;
use phoxal::world::api::session::diagnostics::WorldSessionDiagnostics;
use phoxal::world::api::session::state::WorldSessionState;
use phoxal::world::{WorldSessionHandler, WorldSessionOperation};
use tokio::sync::broadcast;

use super::WorldRuntime;
use crate::attachment::WebotsAttachments;

pub struct WebotsWorldSession {
    runtime: Arc<WorldRuntime>,
    attachments: Arc<WebotsAttachments>,
}

impl WebotsWorldSession {
    pub fn new(runtime: Arc<WorldRuntime>, attachments: Arc<WebotsAttachments>) -> Self {
        Self {
            runtime,
            attachments,
        }
    }
}

impl WorldSessionHandler for WebotsWorldSession {
    fn bootstrap(&self) -> WorldSessionBootstrap {
        self.runtime.bootstrap()
    }

    fn state(&self) -> WorldSessionState {
        self.runtime.snapshot()
    }

    fn subscribe_state(&self) -> broadcast::Receiver<WorldSessionState> {
        self.runtime.subscribe_state()
    }

    fn diagnostics(&self) -> WorldSessionDiagnostics {
        self.runtime.diagnostics()
    }

    fn subscribe_diagnostics(&self) -> broadcast::Receiver<WorldSessionDiagnostics> {
        self.runtime.subscribe_diagnostics()
    }

    fn control(&self, request: WorldControl) -> WorldSessionOperation<'_, WorldSessionState> {
        Box::pin(async move { self.runtime.apply_control(request).await })
    }

    fn attach(
        &self,
        execution: ExecutionId,
        supervisor_endpoint: String,
        spawn: Option<SpawnId>,
    ) -> WorldSessionOperation<'_, WorldSessionState> {
        self.attachments
            .attach(&self.runtime, execution, supervisor_endpoint, spawn)
    }
}
