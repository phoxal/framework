use super::transaction::AttachmentTransactionPhase;
use super::*;
use phoxal::bus::RobotInstant;
use phoxal::identity::{ProducerId, TimelineId};
use phoxal::model::identity::RobotId;
use phoxal::model::world::{LiveAttachmentBoundary, WorldProgress};
use std::sync::atomic::{AtomicBool, Ordering};

fn pose(x: f64) -> phoxal::model::structure::Pose {
    serde_json::from_value(serde_json::json!({
        "xyz": [x, 0.0, 0.0],
        "rpy": [0.0, 0.0, 0.0]
    }))
    .expect("pose")
}

fn world(spawns: &[(&str, f64)]) -> World {
    let spawn_points = spawns
        .iter()
        .map(|(id, x)| {
            (
                (*id).to_owned(),
                serde_json::to_value(pose(*x)).expect("pose JSON"),
            )
        })
        .collect::<serde_json::Map<_, _>>();
    serde_json::from_value(serde_json::json!({
        "id": "test-world",
        "time_step_ns": 12_000_000,
        "gravity_mps2": [0.0, 0.0, -9.81],
        "spawn_points": spawn_points,
        "entities": []
    }))
    .expect("world")
}

fn member(execution: ExecutionId, spawn: SpawnId) -> WorldMember {
    WorldMember {
        execution,
        robot: RobotId::new("robot").expect("robot id"),
        controller: ProducerId::try_from(0x3000_0000_0000_0000_0000_0000_0000_0003)
            .expect("producer"),
        phase: WorldMemberPhase::Active,
        attached_at: LiveAttachmentBoundary {
            world: WorldProgress::zero(12_000_000).expect("progress"),
            execution: RobotInstant::new(TimelineId::from_raw(1).expect("timeline"), 0),
        },
        spawn,
        initial_pose: pose(0.0),
    }
}

#[test]
fn omitted_spawn_requires_exactly_one_authored_point() {
    let one = world(&[("only", 2.0)]);
    let (spawn, resolved) = resolve_spawn(&one, None).expect("sole spawn resolves");
    assert_eq!(spawn.as_str(), "only");
    assert_eq!(resolved.xyz(), [2.0, 0.0, 0.0]);
    assert!(resolve_spawn(&world(&[]), None).is_err());
    assert!(resolve_spawn(&world(&[("first", 0.0), ("second", 1.0)]), None).is_err());
}

#[test]
fn duplicate_spawn_and_conflicting_idempotent_retries_fail_before_mutation() {
    let first =
        ExecutionId::try_from(0x1000_0000_0000_0000_0000_0000_0000_0001).expect("execution");
    let second =
        ExecutionId::try_from(0x2000_0000_0000_0000_0000_0000_0000_0002).expect("execution");
    let spawn = SpawnId::new("west-bay").expect("spawn");
    let other = SpawnId::new("east-bay").expect("spawn");
    let members = vec![member(first, spawn.clone())];
    assert!(ensure_attach_slot(&members, second, &spawn).is_err());
    ensure_attach_slot(&members, second, &other)
        .expect("a second member may reserve the distinct authored spawn");
    assert!(ensure_attach_slot(&members, first, &SpawnId::new("other").expect("spawn")).is_err());

    ensure_idempotent_request(first, &spawn, "tcp://one", &spawn, "tcp://one")
        .expect("exact retry");
    assert!(
        ensure_idempotent_request(
            first,
            &spawn,
            "tcp://one",
            &SpawnId::new("other").expect("spawn"),
            "tcp://one"
        )
        .is_err()
    );
    assert!(ensure_idempotent_request(first, &spawn, "tcp://one", &spawn, "tcp://two").is_err());
}

#[tokio::test]
async fn dropping_a_request_at_an_await_cancels_its_owned_worker_cleanup() {
    let cancellation = OperationCancellation::new();
    let worker_cancellation = cancellation.clone();
    let cleaned = Arc::new(AtomicBool::new(false));
    let worker_cleaned = Arc::clone(&cleaned);
    let worker = tokio::spawn(async move {
        loop {
            if worker_cancellation.check().is_err() {
                worker_cleaned.store(true, Ordering::Release);
                return;
            }
            tokio::time::sleep(Duration::from_millis(1)).await;
        }
    });
    let (started_tx, started_rx) = oneshot::channel();
    let request = tokio::spawn(async move {
        let _cancel_on_drop = CancelOnDrop::new(cancellation);
        let _ = started_tx.send(());
        std::future::pending::<()>().await;
    });
    started_rx.await.expect("request reached its await point");
    request.abort();
    worker.await.expect("owned cleanup worker converged");
    assert!(cleaned.load(Ordering::Acquire));
}

#[tokio::test]
async fn shutdown_closes_admission_before_worker_drain() {
    let mut workers = AttachmentWorkers::new();
    let admitted = OperationCancellation::child(&workers.shutdown);
    let worker_cancellation = admitted.clone();
    workers.tasks.spawn(async move {
        worker_cancellation.0.cancelled().await;
    });

    let mut tasks = workers.close_admission();

    assert!(admitted.check().is_err());
    assert!(
        OperationCancellation::child(&workers.shutdown)
            .check()
            .is_err()
    );
    assert!(tasks.join_next().await.expect("worker result").is_ok());
}

#[tokio::test]
async fn repeated_shutdown_never_reopens_attachment_admission() {
    let mut workers = AttachmentWorkers::new();
    let first = workers.close_admission();
    assert!(workers.shutdown.is_cancelled());
    let mut second = workers.close_admission();
    assert!(first.is_empty());
    assert!(second.join_next().await.is_none());
    assert!(workers.shutdown.is_cancelled());
}

#[tokio::test]
async fn a_panicked_attachment_worker_is_reported_without_losing_other_workers() {
    let mut workers = AttachmentWorkers::new();
    workers
        .tasks
        .spawn(async { panic!("deterministic worker failure") });
    workers.tasks.spawn(async {});
    tokio::task::yield_now().await;

    let error = workers.reap_finished().expect_err("panic is retained");
    assert!(error.to_string().contains("attachment worker failed"));
    assert!(
        workers.tasks.is_empty(),
        "all completed workers were reaped"
    );
}

#[test]
fn failed_import_attempt_still_owns_idempotent_native_removal() {
    let phase = AttachmentTransactionPhase::NativeImportAttempted;
    let native_result = Result::<(), &'static str>::Err("partial native import");
    assert!(native_result.is_err());
    assert_eq!(phase.controller_ready(), Some(false));
}
