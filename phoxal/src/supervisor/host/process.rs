//! Owned child-process lifecycle for a compiled source bundle.
//!
//! The supervisor is the one owner of every required runtime process. A
//! process is launched with the exact bundle root, instance id, and internal
//! Zenoh endpoint that admitted it. Child waiters own their `Child` value and
//! listen to the supervisor's stop token, so a shutdown can kill and await
//! every process without relying on detached threads or cancellable blocking
//! work.

use std::process::Stdio;
use std::time::Duration;

use anyhow::{Context, Result, bail};
use tokio::process::{Child, Command};
use tokio::task::JoinSet;
use tokio::time::timeout;
use tokio_util::sync::CancellationToken;

use crate::supervisor::api::execution::Lifecycle;

use super::bundle::SourceBundle;
use super::state::ExecutionState;

/// Maximum time allowed for all admitted runtime processes to become Ready.
pub(crate) const STARTUP_TIMEOUT: Duration = Duration::from_secs(30);
/// Maximum time allowed to stop all required runtime processes.
pub(crate) const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(10);

/// The supervisor-owned process graph for one source bundle.
pub(crate) struct ProcessSupervisor {
    stop: CancellationToken,
    waits: JoinSet<Result<ChildExit>>,
    instances: usize,
}

#[derive(Debug)]
struct ChildExit {
    instance: String,
    status: std::process::ExitStatus,
}

impl ProcessSupervisor {
    /// Launch every executable recorded by the admitted source bundle.
    pub(crate) async fn launch(
        bundle: &SourceBundle,
        connect: &str,
    ) -> Result<Self> {
        let stop = CancellationToken::new();
        let mut children: Vec<(String, Child)> = Vec::new();
        for executable in bundle.executables() {
            let path = bundle.root().join(executable.path());
            let mut command = Command::new(&path);
            command
                .arg("--bundle-root")
                .arg(bundle.root())
                .arg("--instance-id")
                .arg(executable.instance())
                .arg("--connect")
                .arg(connect)
                .stdin(Stdio::null())
                .stdout(Stdio::inherit())
                .stderr(Stdio::inherit())
                .kill_on_drop(true);
            let child = match command.spawn().with_context(|| {
                format!(
                    "failed to launch runtime `{}` from {}",
                    executable.instance(),
                    path.display()
                )
            }) {
                Ok(child) => child,
                Err(error) => {
                    for (_, mut child) in children {
                        let _ = child.start_kill();
                        let _ = child.wait().await;
                    }
                    return Err(error);
                }
            };
            children.push((executable.instance().to_owned(), child));
        }
        let mut waits = JoinSet::new();
        for (instance, child) in children {
            let stop_for_child = stop.clone();
            waits.spawn(async move { wait_child(child, instance, stop_for_child).await });
        }
        Ok(Self {
            stop,
            waits,
            instances: bundle.executables().count(),
        })
    }

    /// Wait until every launched process has published a Ready lease.
    ///
    /// A process exit before graph readiness is always a startup failure. The
    /// state snapshot is still the authority for readiness because only the
    /// runtime process can prove that it decoded its bundle, validated its
    /// configuration, initialized its state, and joined the execution.
    pub(crate) async fn wait_ready(
        &mut self,
        state: &ExecutionState,
        shutdown: &CancellationToken,
    ) -> Result<()> {
        if self.instances == 0 {
            bail!("source bundle has no launchable runtime processes");
        }
        let mut snapshots = state.subscribe();
        let ready = async {
            loop {
                if snapshots.borrow().lifecycle == Lifecycle::Ready {
                    return Ok::<(), anyhow::Error>(());
                }
                tokio::select! {
                    () = shutdown.cancelled() => {
                        bail!("supervisor shutdown requested before runtime graph became Ready");
                    }
                    joined = self.waits.join_next() => {
                        match joined {
                            Some(Ok(Ok(exit))) => {
                                bail!("runtime `{}` exited before the execution became Ready with status {}", exit.instance, exit.status);
                            }
                            Some(Ok(Err(error))) => return Err(error),
                            Some(Err(error)) => bail!("runtime process waiter failed: {error}"),
                            None => bail!("all runtime processes exited before the execution became Ready"),
                        }
                    }
                    changed = snapshots.changed() => {
                        changed.context("runtime readiness observation ended")?;
                    }
                }
            }
        };
        timeout(STARTUP_TIMEOUT, ready)
            .await
            .context("runtime graph did not become Ready before the startup deadline")??;
        Ok(())
    }

    /// Monitor required children after the execution becomes Ready.
    ///
    /// Any required process exit faults the execution. The caller owns the
    /// resulting shutdown transition and invokes [`Self::stop`] before
    /// releasing the supervisor resources.
    pub(crate) async fn monitor(&mut self, shutdown: &CancellationToken) -> Result<()> {
        loop {
            tokio::select! {
                () = shutdown.cancelled() => return Ok(()),
                joined = self.waits.join_next() => {
                    match joined {
                        Some(Ok(Ok(exit))) => bail!("required runtime `{}` exited with status {}", exit.instance, exit.status),
                        Some(Ok(Err(error))) => return Err(error),
                        Some(Err(error)) => bail!("runtime process waiter failed: {error}"),
                        None => bail!("all required runtime processes exited unexpectedly"),
                    }
                }
            }
        }
    }

    /// Request stop, terminate every child, and await every owned waiter.
    pub(crate) async fn stop(&mut self) -> Result<()> {
        self.stop.cancel();
        let joined = timeout(SHUTDOWN_TIMEOUT, async {
            while let Some(result) = self.waits.join_next().await {
                match result {
                    Ok(Ok(exit)) => {
                        tracing::debug!(instance = %exit.instance, status = %exit.status, "runtime process stopped");
                    }
                    Ok(Err(error)) => return Err(error),
                    Err(error) => return Err(anyhow::anyhow!("runtime process waiter failed: {error}")),
                }
            }
            Ok::<(), anyhow::Error>(())
        })
        .await;
        match joined {
            Ok(result) => result,
            Err(_) => bail!("runtime processes did not stop before the shutdown deadline"),
        }
    }
}

impl Drop for ProcessSupervisor {
    fn drop(&mut self) {
        self.stop.cancel();
        self.waits.abort_all();
    }
}

async fn wait_child(
    mut child: Child,
    instance: String,
    stop: CancellationToken,
) -> Result<ChildExit> {
    let status = tokio::select! {
        result = child.wait() => result.with_context(|| format!("failed waiting for runtime `{instance}`"))?,
        () = stop.cancelled() => {
            // The child may have exited in the same scheduling turn that
            // delivered cancellation. `try_wait` makes that race explicit so
            // a normal stop does not report a spurious kill failure, while a
            // real wait failure remains terminal.
            if child
                .try_wait()
                .with_context(|| format!("failed to inspect runtime `{instance}`"))?
                .is_none()
            {
                child
                    .start_kill()
                    .with_context(|| format!("failed to terminate runtime `{instance}`"))?;
            }
            child
                .wait()
                .await
                .with_context(|| format!("failed waiting for terminated runtime `{instance}`"))?
        }
    };
    Ok(ChildExit { instance, status })
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::time::Duration;

    use tempfile::tempdir;
    use sha2::Digest;

    use super::super::bundle::{SourceBundle, SourceExecutable, SourceManifest};
    use super::super::presence::Presence;
    use super::*;
    use crate::identity::{ParticipantId, ProducerId};
    use crate::participant::metadata::ParticipantKind;

    #[tokio::test]
    async fn stopping_a_started_runtime_terminates_and_joins_it() {
        let temp = tempdir().expect("temporary process bundle");
        let script = temp.path().join("brain");
        fs::write(&script, "#!/bin/sh\nsleep 30\n").expect("write process fixture");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .expect("make process fixture executable");
        let bytes = fs::read(&script).expect("read process fixture");
        let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
        let manifest = SourceManifest::for_test(
            "fixture",
            vec![SourceExecutable::for_test(
                "brain",
                "brain",
                bytes.len() as u64,
                digest,
            )],
        );
        let bundle = SourceBundle::for_test(temp.path(), manifest);
        let mut processes = ProcessSupervisor::launch(&bundle, "test-endpoint")
            .await
            .expect("the process fixture launches");
        tokio::time::sleep(Duration::from_millis(50)).await;
        processes.stop().await.expect("the child is killed and joined");
    }

    #[tokio::test]
    async fn readiness_is_observed_only_after_the_expected_graph_is_present() {
        let temp = tempdir().expect("temporary process bundle");
        let script = temp.path().join("brain");
        fs::write(&script, "#!/bin/sh\nsleep 30\n").expect("write process fixture");
        fs::set_permissions(&script, fs::Permissions::from_mode(0o755))
            .expect("make process fixture executable");
        let bytes = fs::read(&script).expect("read process fixture");
        let digest = format!("{:x}", sha2::Sha256::digest(&bytes));
        let manifest = SourceManifest::for_test(
            "fixture",
            vec![SourceExecutable::for_test(
                "brain",
                "brain",
                bytes.len() as u64,
                digest,
            )],
        );
        let bundle = SourceBundle::for_test(temp.path(), manifest);
        let mut processes = ProcessSupervisor::launch(&bundle, "test-endpoint")
            .await
            .expect("the process fixture launches");
        let state = ExecutionState::new(Presence::for_entries(vec![
            ("brain".to_owned(), ParticipantKind::Brain),
        ])
        .expect("presence graph"))
        .expect("execution state");
        let snapshots = state.clone();
        let stop = CancellationToken::new();
        let mark_ready = tokio::spawn(async move {
            tokio::time::sleep(Duration::from_millis(50)).await;
            snapshots.record_presence(
                &ParticipantId::new("brain").expect("brain identity"),
                ProducerId::try_from((1_u128 << 124) | 1).expect("producer identity"),
                true,
            );
        });
        processes
            .wait_ready(&state, &stop)
            .await
            .expect("presence makes the graph ready");
        mark_ready.await.expect("ready marker task");
        stop.cancel();
        processes.stop().await.expect("the child is killed and joined");
    }
}
