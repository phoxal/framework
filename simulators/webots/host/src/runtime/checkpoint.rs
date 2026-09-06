use super::*;

const CHECKPOINT_QUEUE_CAPACITY: usize = 64;

enum WriterCommand {
    Checkpoint(Box<WorldCheckpoint>),
    Flush(mpsc::Sender<Result<(), String>>),
    Finish(mpsc::Sender<Result<(), String>>),
}

/// Single bounded ordered owner for durable checkpoint writes.
pub(super) struct CheckpointWriter {
    sender: Mutex<Option<mpsc::SyncSender<WriterCommand>>>,
    failure: Arc<Mutex<Option<String>>>,
    worker: Mutex<Option<thread::JoinHandle<()>>>,
}

impl CheckpointWriter {
    pub(super) fn new(evidence: Arc<EvidenceSession>) -> Result<Self, String> {
        Self::with_writer(move |checkpoint| {
            evidence
                .write_checkpoint(checkpoint)
                .map_err(|error| format!("failed to persist world checkpoint: {error:#}"))
        })
    }

    fn with_writer(
        write: impl Fn(&WorldCheckpoint) -> Result<(), String> + Send + 'static,
    ) -> Result<Self, String> {
        let (sender, receiver) = mpsc::sync_channel(CHECKPOINT_QUEUE_CAPACITY);
        let failure = Arc::new(Mutex::new(None));
        let worker_failure = Arc::clone(&failure);
        let worker = thread::Builder::new()
            .name("phoxal-world-checkpoint".to_owned())
            .spawn(move || run_writer(receiver, &worker_failure, write))
            .map_err(|error| format!("failed to start checkpoint writer thread: {error}"))?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            failure,
            worker: Mutex::new(Some(worker)),
        })
    }

    /// Assign queue order without waiting for filesystem I/O.
    pub(super) fn submit(&self, checkpoint: WorldCheckpoint) -> Result<(), String> {
        self.check_failure()?;
        let sender = lock(&self.sender);
        let sender = sender
            .as_ref()
            .ok_or_else(|| "world checkpoint writer is finished".to_owned())?;
        sender
            .try_send(WriterCommand::Checkpoint(Box::new(checkpoint)))
            .map_err(|error| match error {
                mpsc::TrySendError::Full(_) => {
                    "world checkpoint writer queue is saturated".to_owned()
                }
                mpsc::TrySendError::Disconnected(_) => "world checkpoint writer stopped".to_owned(),
            })
    }

    /// Wait until every earlier assigned checkpoint is durable.
    pub(super) fn flush(&self) -> Result<(), String> {
        self.check_failure()?;
        let (acknowledgement, complete) = mpsc::channel();
        {
            let sender = lock(&self.sender);
            sender
                .as_ref()
                .ok_or_else(|| "world checkpoint writer is finished".to_owned())?
                .send(WriterCommand::Flush(acknowledgement))
                .map_err(|_| "world checkpoint writer stopped".to_owned())?;
        }
        complete
            .recv()
            .map_err(|_| "world checkpoint writer stopped before flush".to_owned())??;
        self.check_failure()
    }

    /// Flush, stop, and join the ordered writer before terminal evidence.
    pub(super) fn finish(&self) -> Result<(), String> {
        let mut failures = Vec::new();
        let sender = lock(&self.sender).take();
        if let Some(sender) = sender {
            let (acknowledgement, complete) = mpsc::channel();
            if sender.send(WriterCommand::Finish(acknowledgement)).is_err() {
                failures.push("world checkpoint writer stopped".to_owned());
            } else {
                match complete.recv() {
                    Ok(Ok(())) => {}
                    Ok(Err(error)) => failures.push(error),
                    Err(_) => {
                        failures.push("world checkpoint writer stopped before finish".to_owned())
                    }
                }
            }
        }
        if let Some(worker) = lock(&self.worker).take()
            && worker.join().is_err()
        {
            failures.push("world checkpoint writer panicked".to_owned());
        }
        if let Err(error) = self.check_failure()
            && !failures.contains(&error)
        {
            failures.push(error);
        }
        if failures.is_empty() {
            Ok(())
        } else {
            Err(failures.join("; "))
        }
    }

    fn check_failure(&self) -> Result<(), String> {
        match lock(&self.failure).clone() {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

impl Drop for CheckpointWriter {
    fn drop(&mut self) {
        self.sender
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take();
        if let Some(worker) = self
            .worker
            .get_mut()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .take()
        {
            let _ = worker.join();
        }
    }
}

fn run_writer(
    receiver: mpsc::Receiver<WriterCommand>,
    failure: &Mutex<Option<String>>,
    write: impl Fn(&WorldCheckpoint) -> Result<(), String>,
) {
    while let Ok(command) = receiver.recv() {
        match command {
            WriterCommand::Checkpoint(checkpoint) => {
                if lock(failure).is_none()
                    && let Err(error) = write(&checkpoint)
                {
                    *lock(failure) = Some(error);
                }
            }
            WriterCommand::Flush(acknowledgement) => {
                let result = lock(failure).clone().map_or(Ok(()), Err);
                let _ = acknowledgement.send(result);
            }
            WriterCommand::Finish(acknowledgement) => {
                let result = lock(failure).clone().map_or(Ok(()), Err);
                let _ = acknowledgement.send(result);
                return;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use phoxal::model::identity::WorldId;
    use phoxal::model::world::{WorldDigest, WorldInstanceId, WorldProgress, WorldProvenance};
    use phoxal::version::FrameworkVersion;
    use phoxal::world::api::session::WorldLifecycle;

    fn checkpoint(revision: u64) -> WorldCheckpoint {
        world_checkpoint(
            ProcessIdentity {
                pid: 42,
                started_at_unix_s: 99,
            },
            None,
            WorldSessionState {
                revision,
                instance: WorldInstanceId::parse("10000000000000000000000000000001")
                    .expect("instance"),
                provenance: WorldProvenance {
                    world: WorldId::new("warehouse").expect("world"),
                    digest: WorldDigest::parse(&"0".repeat(64)).expect("digest"),
                    random_seed: 0,
                    framework: FrameworkVersion::CURRENT,
                    adapter: "webots".to_owned(),
                    adapter_version: "test".to_owned(),
                    simulator_version: "R2025a".to_owned(),
                    platform: "test".to_owned(),
                    time_step_ns: 12_000_000,
                },
                lifecycle: WorldLifecycle::Starting,
                progress: WorldProgress::zero(12_000_000).expect("progress"),
                members: Vec::new(),
            },
        )
    }

    #[test]
    fn a_delayed_older_write_cannot_be_overtaken_by_a_newer_revision() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let writes = Arc::clone(&observed);
        let writer = CheckpointWriter::with_writer(move |checkpoint| {
            if checkpoint.state.revision == 1 {
                std::thread::sleep(Duration::from_millis(20));
            }
            lock(&writes).push(checkpoint.state.revision);
            Ok(())
        })
        .expect("writer");
        writer.submit(checkpoint(1)).expect("older admission");
        writer.submit(checkpoint(2)).expect("newer admission");
        writer.finish().expect("terminal flush");
        assert_eq!(*lock(&observed), [1, 2]);
    }

    #[test]
    fn a_write_failure_is_observable_at_the_durability_fence() {
        let writer = CheckpointWriter::with_writer(|_| Err("injected write failure".to_owned()))
            .expect("writer");
        writer.submit(checkpoint(1)).expect("admission");
        assert_eq!(writer.flush().unwrap_err(), "injected write failure");
        assert_eq!(writer.finish().unwrap_err(), "injected write failure");
    }

    #[test]
    fn terminal_finish_flushes_and_refuses_any_later_checkpoint() {
        let observed = Arc::new(Mutex::new(Vec::new()));
        let writes = Arc::clone(&observed);
        let writer = CheckpointWriter::with_writer(move |checkpoint| {
            lock(&writes).push(checkpoint.state.revision);
            Ok(())
        })
        .expect("writer");
        writer.submit(checkpoint(7)).expect("checkpoint admission");
        writer.finish().expect("terminal flush and join");
        assert_eq!(*lock(&observed), [7]);
        assert_eq!(
            writer.submit(checkpoint(8)).unwrap_err(),
            "world checkpoint writer is finished"
        );
    }
}
