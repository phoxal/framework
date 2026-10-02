//! Delivery-observing diagnostics tests for the behavior diary.
//!
//! These tests assert what a real `phoxal::boundary` subscriber actually
//! receives, so they live in their own test target: the observation
//! facility is one shared process-global worker, outbox, and call-site
//! cache, and the tests' subscribers and gated callbacks interleave with
//! each other's emissions through those shared facilities. Isolating the
//! delivery assertions in a dedicated process keeps that shared observer
//! state — not any subscriber API — under one deterministic owner. The
//! production emission path is plain `tracing::debug!` under the
//! dispatcher captured at acceptance; no production tracing workarounds
//! exist to keep these tests green, and ordinary subscribers need no
//! registration-interest overrides.

use phoxal::runtime::behavior::diary::MAX_OUTBOX_RECORDS;
use phoxal::runtime::behavior::{BehaviorDiary, BehaviorRecord, Tree, TreeStatus, wait_until};
use phoxal::runtime::input::InputSnapshot;
use phoxal::runtime::{Context, ExecutionDuration, ExecutionTime, RuntimeOwner, StepContext};
use tracing_subscriber::prelude::*;

/// Serializes the observer tests around their shared observation
/// statics; each test owns the whole window while holding it.
static OBSERVER_TEST_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
/// What each diary-brain invocation observed through its context: the
/// accepted record count and the loss flag at step time.
static OBSERVED: std::sync::Mutex<Vec<(u64, usize, bool)>> = std::sync::Mutex::new(Vec::new());

#[phoxal::messages(package = "phoxal.tests.authoring.diaryobservers.v1")]
mod observer_vocab {
    use phoxal::contracts::RequestReply;

    pub struct Ping {
        #[phoxal(tag = 1)]
        pub nonce: u64,
    }

    pub struct Pong {
        #[phoxal(tag = 1)]
        pub nonce: u64,
    }

    /// The observer brains' endpoint contract.
    #[phoxal::endpoints]
    pub struct ObserverApi {
        #[phoxal::call(
            contract = "phoxal.tests.authoring.diaryobservers.v1.Ping",
            max_items = 8,
            max_bytes = 1_024
        )]
        ping: RequestReply<Ping, Pong>,
    }
}

/// One diary brain: a step-ticked tree inside a real runtime. Each step
/// ticks the tree and then reads the diary's accepted ring and loss flag
/// through its context — the authored runtime is the diagnostic consumer.
pub(crate) struct DiaryBrain {
    tree: Tree<DiaryBrain>,
}

fn diary_tree() -> phoxal::Result<Tree<DiaryBrain>> {
    phoxal::runtime::behavior::Sequence::<DiaryBrain>::new()
        .delay(std::time::Duration::from_millis(10))
        .delay(std::time::Duration::from_millis(10))
        .into_node()
        .within(std::time::Duration::from_secs(60))
        .build()
}

#[phoxal::runtime(contract = observer_vocab::ObserverApi, period_ms = 10)]
impl DiaryBrain {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self {
            tree: diary_tree()?,
        })
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        self.tree.tick(ctx)?;
        if let Some(diary) = ctx.behavior_diary() {
            let accepted = diary.accepted();
            OBSERVED.lock().unwrap_or_else(|e| e.into_inner()).push((
                ctx.invocation_index(),
                accepted.len(),
                diary.overflowed(),
            ));
        }
        Ok(())
    }
}

/// One diary brain whose tree stays running indefinitely: the
/// held-observer test needs many accepted running records.
pub(crate) struct PatientDiaryBrain {
    tree: Tree<PatientDiaryBrain>,
}

#[phoxal::runtime(contract = observer_vocab::ObserverApi, period_ms = 10)]
impl PatientDiaryBrain {
    #[init]
    fn new(_config: ()) -> phoxal::Result<Self> {
        Ok(Self {
            tree: wait_until(|_: &Context<'_, Self>| false).build()?,
        })
    }

    #[step]
    fn advance(&mut self, ctx: &mut Context<'_, Self>) -> phoxal::Result<()> {
        self.tree.tick(ctx)?;
        if let Some(diary) = ctx.behavior_diary() {
            let accepted = diary.accepted();
            OBSERVED.lock().unwrap_or_else(|e| e.into_inner()).push((
                ctx.invocation_index(),
                accepted.len(),
                diary.overflowed(),
            ));
        }
        Ok(())
    }
}

type ObserverInputs = observer_vocab::observer_api::Inputs;

fn inputs() -> ObserverInputs {
    <ObserverInputs as InputSnapshot>::empty()
}

fn context(millis: u64, index: u64) -> StepContext {
    if index == 0 {
        StepContext::first(
            ExecutionTime::from_nanos(millis * 1_000_000),
            ExecutionDuration::from_millis(10),
        )
    } else {
        StepContext::from_previous(
            ExecutionTime::from_nanos(millis * 1_000_000),
            ExecutionDuration::from_millis(10),
            Some(ExecutionTime::from_nanos(
                millis.saturating_sub(10) * 1_000_000,
            )),
            0,
            index,
        )
    }
}

fn diary_owner() -> phoxal::Result<RuntimeOwner<phoxal_runtime_diary_brain::Adapter>> {
    RuntimeOwner::new(
        phoxal_runtime_diary_brain::Adapter::new(),
        ExecutionTime::default(),
        (),
    )
}

fn patient_diary_owner() -> phoxal::Result<RuntimeOwner<phoxal_runtime_patient_diary_brain::Adapter>>
{
    RuntimeOwner::new(
        phoxal_runtime_patient_diary_brain::Adapter::new(),
        ExecutionTime::default(),
        (),
    )
}

/// A capturing boundary layer: an ordinary subscriber that filters by
/// target in `enabled`. No registration-interest override is needed —
/// with differing per-subscriber interests, tracing reports `sometimes`
/// and the active subscriber decides per event, so a disabled control
/// alongside an enabled subscriber remains a valid combination.
struct CaptureLayer {
    buffer: std::sync::Arc<std::sync::Mutex<Vec<String>>>,
    enabled: bool,
}

impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for CaptureLayer {
    fn enabled(
        &self,
        metadata: &tracing::Metadata<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) -> bool {
        self.enabled && metadata.target() == "phoxal::boundary"
    }

    fn on_event(
        &self,
        event: &tracing::Event<'_>,
        _ctx: tracing_subscriber::layer::Context<'_, S>,
    ) {
        struct Visitor<'a>(&'a mut String);
        impl tracing::field::Visit for Visitor<'_> {
            fn record_debug(
                &mut self,
                _field: &tracing::field::Field,
                value: &dyn std::fmt::Debug,
            ) {
                *self.0 = format!("{value:?}");
            }
        }
        let mut payload = String::new();
        event.record(&mut Visitor(&mut payload));
        self.buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(format!(
                "{} {:?} {}",
                event.metadata().target(),
                event.metadata().level(),
                payload,
            ));
    }
}

/// The published record reaches the existing `phoxal::boundary` trace
/// facility: a scoped subscriber captures the real emitted events for
/// accepted invocations, a subscriber that filters the target out
/// observes nothing while execution stays unchanged, and delivery is
/// asynchronous — the observer sees the records after acceptance, never
/// during it.
#[test]
fn observer_output_reaches_the_boundary_trace_facility() -> phoxal::Result<()> {
    let _guard = OBSERVER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    // Disabled half: the target filtered out — invocations still accept,
    // statuses still progress, and nothing is delivered.
    let disabled_buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let subscriber = tracing_subscriber::registry().with(CaptureLayer {
            buffer: std::sync::Arc::clone(&disabled_buffer),
            enabled: false,
        });
        let _scope = tracing::subscriber::set_default(subscriber);
        let mut owner = diary_owner()?;
        for tick in 0..2_u64 {
            owner.accept(&context(tick * 10, tick), &inputs())?;
        }
        assert_eq!(
            disabled_buffer
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .len(),
            0,
            "a subscriber that filters the boundary target observes nothing"
        );
        drop(owner);
    }
    // Enabled half: two accepted invocations deliver their records through
    // the boundary facility. Emission happens on the observation
    // facility's worker, so the observer sees them shortly after
    // acceptance.
    let enabled_buffer = std::sync::Arc::new(std::sync::Mutex::new(Vec::new()));
    {
        let subscriber = tracing_subscriber::registry().with(CaptureLayer {
            buffer: std::sync::Arc::clone(&enabled_buffer),
            enabled: true,
        });
        let _scope = tracing::subscriber::set_default(subscriber);
        let mut owner = diary_owner()?;
        for tick in 0..2_u64 {
            owner.accept(&context(tick * 10, tick), &inputs())?;
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while enabled_buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .len()
            < 2
        {
            assert!(
                std::time::Instant::now() < deadline,
                "the boundary events never reached the observer"
            );
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        let captured = enabled_buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .join("\n");
        assert!(
            captured.contains("phoxal::boundary"),
            "the behavior record reached the existing boundary facility: {captured}"
        );
        assert!(
            captured.contains("behavior"),
            "the emitted record names the behavior event: {captured}"
        );
        assert!(
            captured.contains("within.sequence[0].delay"),
            "the emitted record carries the stable active path: {captured}"
        );
        assert!(
            captured.contains("within.sequence[1].delay"),
            "the second accepted record carries its own path: {captured}"
        );
        assert!(
            captured.contains("\"epoch\":"),
            "the emitted record carries its explicit diagnostic epoch: {captured}"
        );
        drop(owner);
    }
    Ok(())
}

/// A blocked observer never blocks acceptance or teardown: with a real
/// boundary subscriber parked inside its event callback, the runtime
/// owner keeps accepting far past the observation outbox's bound, the
/// saturated handoff becomes visible as a loss flag without changing
/// behavior, and dropping the runtime-owned adapter completes while the
/// callback is still held.
#[test]
fn a_held_observer_never_blocks_acceptance_or_teardown() -> phoxal::Result<()> {
    struct GateLayer {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for GateLayer {
        fn on_event(
            &self,
            _event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            let _ = self.entered.send(());
            let _ = self
                .release
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .recv();
        }
    }

    let _guard = OBSERVER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    OBSERVED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let subscriber = tracing_subscriber::registry().with(GateLayer {
        entered: entered_tx,
        release: std::sync::Mutex::new(release_rx),
    });
    let _scope = tracing::subscriber::set_default(subscriber);
    let mut owner = patient_diary_owner()?;
    // Park the observation facility inside the observer on the first
    // delivered record, then drive far past the outbox's bound.
    let invocations = MAX_OUTBOX_RECORDS as u64 * 2 + 8;
    let start = std::time::Instant::now();
    for tick in 0..invocations {
        owner.accept(&context(tick * 10, tick), &inputs())?;
    }
    let held = start.elapsed();
    assert!(
        entered_rx
            .recv_timeout(std::time::Duration::from_secs(1))
            .is_ok(),
        "the held observer did receive the real boundary event"
    );
    assert!(
        held < std::time::Duration::from_secs(10),
        "acceptance waited {held:?} for an observer that never returned"
    );
    // The saturated handoff is visible: the final invocation's step
    // observed the loss flag through its own context.
    let observed = OBSERVED.lock().unwrap_or_else(|e| e.into_inner());
    let (_, _, overflow) = observed.last().copied().unwrap_or((0, 0, false));
    assert!(
        overflow,
        "bounded observer backpressure must be visible, not silent"
    );
    drop(observed);
    // Teardown is independent of arbitrary observer code: dropping the
    // runtime-owned adapter completes while the callback is still held.
    // The facility drains afterwards, once the gate is released once per
    // parked/queued record.
    let teardown = std::time::Instant::now();
    drop(owner);
    let tore_down = teardown.elapsed();
    assert!(
        tore_down < std::time::Duration::from_secs(1),
        "diary teardown waited {tore_down:?} for a held observer"
    );
    for _ in 0..invocations {
        let _ = release_tx.send(());
    }
    drop(_scope);
    Ok(())
}

/// An observer that panics inside its callback can neither fault the
/// runtime nor stop later records from flowing: the panic is contained
/// on the observation facility's worker, the handoff keeps serving, and
/// the delivery loss is visible through the diary's loss flag.
#[test]
fn an_observer_panic_is_contained_and_marks_visible_loss() -> phoxal::Result<()> {
    struct PanickingLayer;
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for PanickingLayer {
        fn on_event(
            &self,
            _event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            panic!("the observer exploded");
        }
    }

    let _guard = OBSERVER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    OBSERVED.lock().unwrap_or_else(|e| e.into_inner()).clear();
    let _scope =
        tracing::subscriber::set_default(tracing_subscriber::registry().with(PanickingLayer));
    let mut owner = diary_owner()?;
    for tick in 0..4_u64 {
        owner.accept(&context(tick * 10, tick), &inputs())?;
    }
    // The panic is contained on the observation facility's worker, so its
    // loss marking becomes visible asynchronously: keep accepting (the
    // panicking observer can neither fault the owner nor stop records)
    // until a step observes the loss flag.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
    let mut overflow = false;
    let mut tick = 4_u64;
    while std::time::Instant::now() < deadline {
        owner.accept(&context(tick * 10, tick), &inputs())?;
        tick += 1;
        let observed = OBSERVED.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((_, _, seen)) = observed.last().copied() {
            overflow = seen;
        }
        drop(observed);
        if overflow {
            break;
        }
    }
    assert!(
        overflow,
        "a caught observer panic is visible delivery loss, not a silent drop"
    );
    drop(owner);
    drop(_scope);
    let observed = OBSERVED.lock().unwrap_or_else(|e| e.into_inner());
    assert!(
        observed.len() > 4,
        "accepted records keep flowing despite the panicking observer"
    );
    Ok(())
}

/// Reset fencing distinguishes queued records from one already inside a
/// subscriber callback: after `clear`, queued old-generation records
/// never reach the observer, and the one in-flight record completes
/// delivery explicitly labeled with its pre-reset epoch.
#[test]
fn reset_fences_queued_records_and_labels_the_inflight_one() -> phoxal::Result<()> {
    use std::sync::atomic::{AtomicBool, Ordering};

    struct GateFirstLayer {
        entered: std::sync::mpsc::Sender<()>,
        release: std::sync::Mutex<std::sync::mpsc::Receiver<()>>,
        events: std::sync::mpsc::Sender<String>,
        holding: AtomicBool,
    }
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for GateFirstLayer {
        // The gate holds the FIRST on_event only: on_event runs solely
        // for real dispatches — never during call-site interest
        // reevaluation, where layer `enabled` callbacks also run — so
        // the hold is deterministic while still parking the facility
        // mid-delivery of the first record.
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _ctx: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if self.holding.swap(false, Ordering::SeqCst) {
                let _ = self.entered.send(());
                let _ = self
                    .release
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .recv();
            }
            struct Visitor<'a>(&'a mut String);
            impl tracing::field::Visit for Visitor<'_> {
                fn record_debug(
                    &mut self,
                    _field: &tracing::field::Field,
                    value: &dyn std::fmt::Debug,
                ) {
                    *self.0 = format!("{value:?}");
                }
            }
            let mut payload = String::new();
            event.record(&mut Visitor(&mut payload));
            let _ = self.events.send(payload);
        }
    }

    let _guard = OBSERVER_TEST_LOCK.lock().unwrap_or_else(|e| e.into_inner());
    let (entered_tx, entered_rx) = std::sync::mpsc::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel();
    let (event_tx, event_rx) = std::sync::mpsc::channel();
    let diary = BehaviorDiary::default();
    {
        let subscriber = tracing_subscriber::registry().with(GateFirstLayer {
            entered: entered_tx,
            release: std::sync::Mutex::new(release_rx),
            events: event_tx,
            holding: AtomicBool::new(true),
        });
        let _scope = tracing::subscriber::set_default(subscriber);
        let record = |invocation: u64| BehaviorRecord {
            generation: 7,
            invocation,
            time_ns: 0,
            status: TreeStatus::Running,
            kind: phoxal::runtime::behavior::FailureKind::None,
            path: "sequence[0].wait".to_owned(),
            pending_calls: 0,
            active_captures: 0,
        };
        // The first record parks the facility inside its delivery
        // callback.
        diary.stage(record(0));
        diary.promote_staged();
        assert!(
            entered_rx
                .recv_timeout(std::time::Duration::from_secs(1))
                .is_ok(),
            "the first record reached the held observer callback"
        );
        // Three more records queue behind the parked one.
        for invocation in 1..4_u64 {
            diary.stage(record(invocation));
            diary.promote_staged();
        }
    }
    // Reset while the first record is still inside the callback and three
    // old-generation records sit queued.
    diary.clear();
    let _ = release_tx.send(());
    // The in-flight record completes delivery, explicitly labeled with
    // its pre-reset epoch (0): it can be identified as old progress,
    // never mistaken for the new generation.
    let inflight = event_rx
        .recv_timeout(std::time::Duration::from_secs(2))
        .map_err(|e| phoxal::anyhow!("the in-flight record never completed: {e}"))?;
    assert!(
        inflight.contains("\"epoch\":0"),
        "the in-flight record must carry its pre-reset epoch: {inflight}"
    );
    // The queued old-generation records were dropped before dispatch.
    assert!(
        event_rx
            .recv_timeout(std::time::Duration::from_millis(200))
            .is_err(),
        "queued old-generation records must never reach the observer"
    );
    Ok(())
}
