use super::*;
use crate::shutdown::{await_world_controller_stop, failing_identity, terminal_outcome};

struct HostApplication {
    bundle: WorldBundle,
    instance: WorldInstanceId,
    registry_root: PathBuf,
    runtime: Arc<WorldRuntime>,
    session: Arc<WebotsWorldSession>,
    attachments: Arc<WebotsAttachments>,
    native: Arc<HostServer>,
    process: registration::ProcessIdentity,
    public: Option<WorldSessionServer>,
    registration: Option<RegistrationGuard>,
}

pub(super) async fn run(args: Args, log_byte_limit: u64, host_log: BoundedStderr) -> Result<()> {
    let bundle = WorldBundle::open(&args.world_bundle).with_context(|| {
        format!(
            "failed to open world bundle {}",
            args.world_bundle.display()
        )
    })?;
    let instance = WorldInstanceId::mint();
    let evidence_root = required_path(EVIDENCE_DIRECTORY_ENV)?;
    let registry_root = required_path(REGISTRY_DIRECTORY_ENV)?;
    let evidence = Arc::new(EvidenceSession::create(
        &evidence_root,
        instance,
        &bundle,
        log_byte_limit,
    )?);
    let process = current_process_identity()?;
    let installation = WebotsInstallation::discover()?;
    let native = Arc::new(HostServer::bind()?);
    let executable_directory = std::env::current_exe()?
        .parent()
        .context("host executable has no containing directory")?
        .to_path_buf();
    let controllers = ControllerExecutables {
        world: executable_directory.join(WORLD_CONTROLLER_PACKAGE),
        robot: executable_directory.join(ROBOT_CONTROLLER_PACKAGE),
    };
    let staging = tempfile::Builder::new()
        .prefix("phoxal-webots-")
        .tempdir()
        .context("failed to create the native Webots staging directory")?;
    let project_root = staging.path().join("project");
    let project = stage_project(&bundle, &project_root, native.endpoint(), &controllers)?;
    let attachments = Arc::new(WebotsAttachments::new(
        instance,
        bundle.world().clone(),
        project_root,
        Arc::clone(&native),
        Arc::clone(&evidence),
    ));
    let runtime = Arc::new(
        WorldRuntime::new(
            instance,
            &bundle,
            installation.version(),
            Arc::clone(&native),
            Arc::clone(&evidence),
            process,
        )
        .map_err(anyhow::Error::msg)?,
    );
    let webots_limit = log_byte_limit.saturating_sub(log_byte_limit / 2).max(1);
    let mut webots = WebotsProcess::launch(
        &installation,
        project.world(),
        &evidence.webots_log(),
        webots_limit,
        false,
    )?;
    evidence.set_native_process(webots.identity()?);
    runtime.refresh_checkpoint().map_err(anyhow::Error::msg)?;
    let session = Arc::new(WebotsWorldSession::new(
        Arc::clone(&runtime),
        Arc::clone(&attachments),
    ));

    let mut application = HostApplication {
        bundle,
        instance,
        registry_root,
        runtime: Arc::clone(&runtime),
        session,
        attachments: Arc::clone(&attachments),
        native: Arc::clone(&native),
        process,
        public: None,
        registration: None,
    };
    let live_result = application.serve_live(&mut webots).await;
    let state_before_cleanup = runtime.snapshot();
    let native_before_cleanup = native.snapshot();
    let members_before_cleanup = state_before_cleanup.members.clone();
    let terminal_reason = match state_before_cleanup.lifecycle {
        WorldLifecycle::Failed { reason } => Some(reason),
        WorldLifecycle::Starting | WorldLifecycle::Ready { .. } | WorldLifecycle::Stopping => None,
    };
    let end_reason = match state_before_cleanup.lifecycle {
        WorldLifecycle::Failed { reason } => reason,
        _ => SimulationEndReason::WorldStopped,
    };
    let member_cleanup_detail = attachments
        .end_all(&runtime, end_reason)
        .await
        .err()
        .map(|error| format!("{error:#}"));
    native.stop_world();
    let controller_cleanup_detail = await_world_controller_stop(&native, &mut webots)
        .await
        .err()
        .map(|error| format!("{error:#}"));
    let stopped = webots.stop().await;
    let (webots_log, native_cleanup_detail) = match stopped {
        Ok(outcome) => (outcome, None),
        Err(error) => (
            LogCaptureOutcome {
                bytes: 0,
                truncated: true,
            },
            Some(format!("{error:#}")),
        ),
    };
    let public_cleanup_detail = match application.public.take() {
        Some(public) => public
            .close()
            .await
            .err()
            .map(|error| format!("failed to close public world-session endpoint: {error}")),
        None => None,
    };
    let evidence_writer_detail = runtime
        .finish_evidence_writer()
        .err()
        .map(|error| format!("failed to flush checkpoint evidence: {error}"));
    let cleanup_detail = [
        member_cleanup_detail.clone(),
        controller_cleanup_detail.clone(),
        native_cleanup_detail.clone(),
        public_cleanup_detail.clone(),
        evidence_writer_detail.clone(),
    ]
    .into_iter()
    .flatten()
    .reduce(|left, right| format!("{left}; {right}"));
    let cleanup_failure_reason = cleanup_detail.as_ref().map(|_| {
        if member_cleanup_detail.is_some() {
            SimulationEndReason::RemovalFailed
        } else if controller_cleanup_detail.is_some() {
            SimulationEndReason::WorldControllerLost
        } else if native_cleanup_detail.is_some() {
            SimulationEndReason::SimulatorLost
        } else {
            SimulationEndReason::ProtocolViolation
        }
    });
    let failure_reason = terminal_reason.or(cleanup_failure_reason);
    let failing = failing_identity(
        failure_reason,
        native_before_cleanup.lifecycle(),
        &members_before_cleanup,
        evidence.native_process().as_ref(),
    );
    let outcome = terminal_outcome(
        live_result.as_ref().err().map(|error| format!("{error:#}")),
        failure_reason,
        cleanup_detail.clone(),
    );
    let mut truncated = Vec::new();
    if host_log.truncated() {
        truncated.push("host.log".to_owned());
    }
    if webots_log.truncated {
        truncated.push("webots.log".to_owned());
    }
    evidence.write_summary(&world_terminal_summary(
        instance,
        state_before_cleanup.provenance,
        outcome,
        runtime.snapshot().progress,
        members_before_cleanup,
        evidence.member_evidence()?,
        failing,
        TerminalCleanup {
            complete: cleanup_detail.is_none(),
            detail: cleanup_detail.clone(),
        },
        TerminalRetention {
            log_byte_limit,
            truncated,
        },
    ))?;
    // Keep the owner-held lease discoverable until every native/member authority has converged
    // and terminal evidence is atomically durable. Registration is removed last.
    drop(application.registration.take());
    match (live_result, cleanup_detail) {
        (Ok(()), None) => Ok(()),
        (Ok(()), Some(detail)) => bail!("terminal cleanup failed: {detail}"),
        (Err(error), _) => Err(error),
    }
}

impl HostApplication {
    async fn serve_live(&mut self, webots: &mut WebotsProcess) -> Result<()> {
        // The CLI rolls back a transaction-owned host with SIGTERM. Install its
        // handler before publishing readiness so rollback uses the same cleanup
        // path and retained evidence as an explicit world stop.
        let mut terminate =
            tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
                .context("failed to observe world-host termination")?;
        let deadline = tokio::time::Instant::now() + BOOTSTRAP_TIMEOUT;
        loop {
            if let Some(status) = webots.exited()? {
                return fail_for_webots_exit(
                    &self.runtime,
                    format!("Webots exited during bootstrap with {status}"),
                );
            }
            self.native.enforce_liveness();
            let snapshot = self.native.snapshot();
            match snapshot.lifecycle() {
                NativeWorldLifecycle::Ready { .. } => break,
                NativeWorldLifecycle::Failed(reason) => {
                    self.runtime
                        .reconcile_latest_native()
                        .map_err(anyhow::Error::msg)?;
                    bail!("native Webots bootstrap failed: {reason:?}");
                }
                NativeWorldLifecycle::Starting | NativeWorldLifecycle::Stopping => {}
            }
            if tokio::time::Instant::now() >= deadline {
                self.runtime
                    .fail(SimulationEndReason::WorldControllerLost)
                    .map_err(anyhow::Error::msg)?;
                bail!("Webots world controller did not become ready within {BOOTSTRAP_TIMEOUT:?}");
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        self.runtime.mark_ready().map_err(anyhow::Error::msg)?;
        let public = WorldSessionServer::bind(Arc::clone(&self.session))
            .await
            .context("failed to bind the public world-session endpoint")?;
        let registration = RegistrationGuard::create(
            &self.registry_root,
            self.instance,
            public.endpoint().to_owned(),
            &self.bundle,
            self.process,
        )?;
        let endpoint = public.endpoint().to_owned();
        self.public = Some(public);
        self.registration = Some(registration);
        println!("{}", self.instance);
        std::io::stdout()
            .flush()
            .context("failed to publish the world ready line")?;
        tracing::info!(
            instance = %self.instance,
            world = %self.bundle.world().id(),
            digest = %self.bundle.digest(),
            endpoint,
            "native Webots world is ready and paused"
        );

        let mut interval = tokio::time::interval(RECONCILE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            tokio::select! {
                signal = tokio::signal::ctrl_c() => {
                    signal.context("failed to wait for the stop signal")?;
                    self.runtime.mark_stopping().map_err(anyhow::Error::msg)?;
                }
                _ = terminate.recv() => {
                    self.runtime.mark_stopping().map_err(anyhow::Error::msg)?;
                }
                _ = interval.tick() => {}
            }
            if let Some(status) = webots.exited()? {
                return fail_for_webots_exit(
                    &self.runtime,
                    format!("Webots exited unexpectedly with {status}"),
                );
            }
            self.native.enforce_liveness();
            let snapshot = self
                .runtime
                .reconcile_latest_native()
                .map_err(anyhow::Error::msg)?;
            match self.runtime.snapshot().lifecycle {
                WorldLifecycle::Stopping => break,
                WorldLifecycle::Failed { reason } => {
                    bail!(
                        "native world failed: {reason:?}; {:?}",
                        snapshot.lifecycle()
                    );
                }
                WorldLifecycle::Starting | WorldLifecycle::Ready { .. } => {}
            }
            self.attachments.reconcile_removals(&self.runtime).await?;
        }
        Ok(())
    }
}

fn fail_for_webots_exit(runtime: &WorldRuntime, detail: String) -> Result<()> {
    if let Err(error) = runtime.fail(SimulationEndReason::SimulatorLost) {
        bail!("{detail}; failed to publish SimulatorLost: {error}");
    }
    bail!("{detail}")
}

fn required_path(name: &str) -> Result<PathBuf> {
    std::env::var_os(name)
        .map(PathBuf::from)
        .with_context(|| format!("required environment variable {name} is missing"))
}
