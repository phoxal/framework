use super::*;

#[derive(Clone)]
pub(super) struct OperationCancellation(pub(super) CancellationToken);

pub(crate) struct AttachmentWorkers {
    pub(super) shutdown: CancellationToken,
    pub(super) tasks: JoinSet<()>,
}

impl AttachmentWorkers {
    pub(super) fn new() -> Self {
        Self {
            shutdown: CancellationToken::new(),
            tasks: JoinSet::new(),
        }
    }

    pub(super) fn reap_finished(&mut self) -> Result<()> {
        let mut failures = Vec::new();
        while let Some(result) = self.tasks.try_join_next() {
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("attachment worker failed: {}", failures.join("; "))
        }
    }

    pub(super) fn close_admission(&mut self) -> JoinSet<()> {
        self.shutdown.cancel();
        std::mem::take(&mut self.tasks)
    }
}

impl OperationCancellation {
    pub(super) fn child(parent: &CancellationToken) -> Self {
        Self(parent.child_token())
    }

    #[cfg(test)]
    pub(super) fn new() -> Self {
        Self(CancellationToken::new())
    }

    pub(super) fn check(&self) -> Result<()> {
        ensure!(
            !self.0.is_cancelled(),
            "world attachment operation was cancelled"
        );
        Ok(())
    }

    pub(super) fn cancel(&self) {
        self.0.cancel();
    }
}

pub(super) struct CancelOnDrop {
    cancellation: OperationCancellation,
    armed: bool,
}

impl CancelOnDrop {
    pub(super) fn new(cancellation: OperationCancellation) -> Self {
        Self {
            cancellation,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for CancelOnDrop {
    fn drop(&mut self) {
        if self.armed {
            self.cancellation.cancel();
        }
    }
}

impl WebotsAttachments {
    pub(super) async fn cancel_and_join_workers(&self) -> Result<()> {
        let mut failures = Vec::new();
        let mut tasks = {
            let mut workers = self.workers.lock().await;
            if let Err(error) = workers.reap_finished() {
                failures.push(error.to_string());
            }
            workers.close_admission()
        };
        while let Some(result) = tasks.join_next().await {
            if let Err(error) = result {
                failures.push(error.to_string());
            }
        }
        if failures.is_empty() {
            Ok(())
        } else {
            anyhow::bail!("attachment worker failed: {}", failures.join("; "))
        }
    }
}
