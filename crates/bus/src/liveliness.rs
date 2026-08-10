//! Participant presence built on Zenoh Liveliness.
//!
//! Each token is keyed by execution root, participant id, and producer
//! identity. Observers receive exact per-producer events and can aggregate them
//! by the stable participant id for present/not-present UI state. Because the
//! root is execution-scoped, a previous run's tokens are on different keys
//! entirely and can never be mistaken for the current run's.

use phoxal_runtime_contract::identity::{ParticipantId, ProducerId};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc;
use zenoh::key_expr::OwnedKeyExpr;
use zenoh::sample::SampleKind;

use crate::error::{BusError, KeyProblem, Result};
use crate::metadata::ParticipantSourceIdentity;
use crate::session::{BusHandle, BusOwner};

pub(crate) const PARTICIPANT_LIVELINESS_PREFIX: &str = "liveliness/participants";

/// One stable participant Ready key in the Zenoh Liveliness transport.
#[derive(Clone, Debug, PartialEq, Eq)]
struct ParticipantReadyKey {
    key: OwnedKeyExpr,
    source: ParticipantSourceIdentity,
}

impl ParticipantReadyKey {
    pub(crate) fn for_bus(bus: &BusHandle) -> Result<Self> {
        let participant = bus
            .participant()
            .ok_or_else(|| BusError::invalid_key("", KeyProblem::Empty))?;
        Self::new(bus.root(), participant.as_str(), bus.producer())
    }

    /// Build and validate a producer-qualified participant key below an
    /// existing execution root.
    fn new(root: &str, participant: impl Into<String>, producer: ProducerId) -> Result<Self> {
        validate_root(root)?;
        let participant = participant.into();
        validate_participant(&participant)?;
        let participant = ParticipantId::new(participant.clone())
            .map_err(|_| BusError::invalid_key(participant, KeyProblem::NotOneSegment))?;
        let source = ParticipantSourceIdentity::new(participant, producer);
        let raw = format!(
            "{root}/{PARTICIPANT_LIVELINESS_PREFIX}/{}/{}",
            source.participant, source.producer
        );
        let key = OwnedKeyExpr::new(raw.clone())
            .map_err(|error| BusError::not_a_key_expression(&raw, error))?;
        Ok(Self { key, source })
    }

    /// The complete execution-rooted Zenoh key.
    fn as_str(&self) -> &str {
        self.key.as_str()
    }

    /// Participant id encoded in the key.
    #[cfg(test)]
    fn participant(&self) -> &ParticipantId {
        &self.source.participant
    }

    /// Producer identity encoded in the key.
    #[cfg(test)]
    fn producer(&self) -> ProducerId {
        self.source.producer
    }

    /// The participant/source pair encoded in the key.
    fn source(&self) -> &ParticipantSourceIdentity {
        &self.source
    }

    /// Parse a concrete participant key emitted below `root`.
    fn parse(root: &str, key: &str) -> Option<Self> {
        let suffix = key.strip_prefix(root)?.strip_prefix('/')?;
        let suffix = suffix
            .strip_prefix(PARTICIPANT_LIVELINESS_PREFIX)?
            .strip_prefix('/')?;
        let (participant, producer) = suffix.split_once('/')?;
        if producer.contains('/') {
            return None;
        }
        Self::new(root, participant, ProducerId::parse(producer).ok()?).ok()
    }

    /// Wildcard selector used by an execution-scoped observer.
    fn selector(root: &str) -> Result<OwnedKeyExpr> {
        validate_root(root)?;
        let selector = format!("{root}/{PARTICIPANT_LIVELINESS_PREFIX}/*/*");
        OwnedKeyExpr::new(selector.clone())
            .map_err(|error| BusError::not_a_key_expression(&selector, error))
    }

    /// Selector for one participant's producer-qualified Ready keys.
    fn participant_selector(root: &str, participant: &ParticipantId) -> Result<OwnedKeyExpr> {
        validate_root(root)?;
        validate_participant(participant.as_str())?;
        let selector = format!("{root}/{PARTICIPANT_LIVELINESS_PREFIX}/{participant}/*");
        OwnedKeyExpr::new(selector.clone())
            .map_err(|error| BusError::not_a_key_expression(&selector, error))
    }
}

/// Presence or absence of one Liveliness token.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LivelinessStatus {
    /// The token is declared.
    Alive,
    /// The token is not declared, or stopped being declared.
    Lost,
}

/// Whether one exact participant producer currently owns its Ready lease.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParticipantReadyStatus {
    /// The producer-qualified Ready lease is present.
    Ready,
    /// The producer-qualified Ready lease disappeared.
    Lost,
}

impl From<SampleKind> for ParticipantReadyStatus {
    fn from(kind: SampleKind) -> Self {
        match kind {
            SampleKind::Put => ParticipantReadyStatus::Ready,
            SampleKind::Delete => ParticipantReadyStatus::Lost,
        }
    }
}

impl From<SampleKind> for LivelinessStatus {
    fn from(kind: SampleKind) -> Self {
        match kind {
            SampleKind::Put => LivelinessStatus::Alive,
            SampleKind::Delete => LivelinessStatus::Lost,
        }
    }
}

/// A participant Ready change for one exact producer incarnation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParticipantReadyEvent {
    /// The stable participant role and exact process/session incarnation that
    /// declared Ready.
    pub source: ParticipantSourceIdentity,
    /// Whether that exact Ready lease appeared or disappeared.
    pub status: ParticipantReadyStatus,
}

impl ParticipantReadyEvent {
    /// The stable participant role represented by this event.
    pub fn participant(&self) -> &ParticipantId {
        &self.source.participant
    }

    /// The exact producer incarnation represented by this event.
    pub fn producer(&self) -> ProducerId {
        self.source.producer
    }
}

/// Keeps this participant producer's Ready lease alive until it is dropped.
pub struct ParticipantReadyToken {
    _token: zenoh::liveliness::LivelinessToken,
    source: ParticipantSourceIdentity,
}

/// Keeps one infrastructure-owned exact Liveliness key declared until drop.
/// Participant readiness uses [`ParticipantReadyToken`] instead; this token is
/// for execution-scoped authorities such as the supervisor presence lease.
pub struct KeyLivelinessToken {
    _token: zenoh::liveliness::LivelinessToken,
    relative_key: String,
}

impl KeyLivelinessToken {
    /// The exact key below this token's execution root.
    pub fn relative_key(&self) -> &str {
        &self.relative_key
    }
}

impl ParticipantReadyToken {
    /// The participant/source pair represented by this lease.
    pub fn source(&self) -> &ParticipantSourceIdentity {
        &self.source
    }

    /// The stable participant role represented by this lease.
    pub fn participant(&self) -> &ParticipantId {
        &self.source.participant
    }

    /// The exact producer incarnation represented by this lease.
    pub fn producer(&self) -> ProducerId {
        self.source.producer
    }
}

/// Keeps a history-enabled participant Ready observer declared until dropped.
pub struct ParticipantReadyObserver {
    _subscriber: zenoh::pubsub::Subscriber<()>,
}

/// A bounded runner-facing stream of exact participant Ready changes.
///
/// Zenoh invokes the observer callback outside the participant step loop.  The
/// callback therefore only attempts a bounded local enqueue; services drain
/// this value at their ordinary step boundary.  If the queue ever overflows,
/// [`Self::overflowed`] remains true and fixed-source consumers must fail
/// closed rather than act on an incomplete Ready set.
pub struct ParticipantReadyEvents {
    receiver: Mutex<mpsc::Receiver<ParticipantReadyEvent>>,
    _observer: ParticipantReadyObserver,
    overflowed: Arc<AtomicBool>,
}

impl ParticipantReadyEvents {
    /// Drain one Ready event without waiting.
    pub fn try_recv(&self) -> Option<ParticipantReadyEvent> {
        match self.receiver.lock() {
            Ok(mut receiver) => receiver.try_recv().ok(),
            Err(_) => {
                // A poisoned receiver means the exact Ready set can no
                // longer be trusted. Fixed-source consumers observe this as
                // overflow and stop admitting authority.
                self.overflowed.store(true, Ordering::Release);
                None
            }
        }
    }

    /// Whether the bounded observer queue dropped an event.
    pub fn overflowed(&self) -> bool {
        self.overflowed.load(Ordering::Acquire)
    }
}

/// Keeps an observation of one exact Liveliness key declared until it is
/// dropped, and carries the state that key was in when it was established.
pub struct KeyLivelinessObserver {
    _subscriber: zenoh::pubsub::Subscriber<()>,
    initial: LivelinessStatus,
}

impl KeyLivelinessObserver {
    /// The token's state at the moment this observation was established.
    ///
    /// A client that attached after the token was declared learns it is there
    /// from this, not from a change event that already happened.
    pub fn initial(&self) -> LivelinessStatus {
        self.initial
    }
}

impl BusOwner {
    /// Declare one exact execution-scoped infrastructure presence lease.
    ///
    /// The key must be a concrete relative key. This cannot mint participant
    /// Ready authority; that producer-qualified contract has its own method.
    pub async fn declare_liveliness_key(&self, relative_key: &str) -> Result<KeyLivelinessToken> {
        validate_relative_key(relative_key)?;
        if relative_key.starts_with(PARTICIPANT_LIVELINESS_PREFIX) {
            return Err(BusError::invalid_key(
                relative_key,
                KeyProblem::ReservedPrefix,
            ));
        }
        let bus = self.handle();
        let token = bus
            .session()?
            .liveliness()
            .declare_token(bus.full_key(relative_key))
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        Ok(KeyLivelinessToken {
            _token: token,
            relative_key: relative_key.to_string(),
        })
    }

    /// Declare this bus participant's Ready lease. Call only after setup succeeds.
    pub async fn declare_participant_ready(&self) -> Result<ParticipantReadyToken> {
        let bus = self.handle();
        let key = ParticipantReadyKey::for_bus(&bus)?;
        let token = bus
            .session()?
            .liveliness()
            .declare_token(key.as_str())
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        Ok(ParticipantReadyToken {
            _token: token,
            source: key.source().clone(),
        })
    }
}

impl BusHandle {
    /// Observe all exact participant Ready tokens below this execution root,
    /// delivering changes through a bounded local channel suitable for a
    /// participant step loop.
    pub async fn participant_ready_events(&self) -> Result<ParticipantReadyEvents> {
        self.participant_ready_events_with_selector(ParticipantReadyKey::selector(self.root())?)
            .await
    }

    /// Observe only the producer-qualified Ready keys for `participant`.
    /// Scoping the selector before the bounded local queue prevents unrelated
    /// participant churn from consuming the receiver's authority evidence.
    pub async fn participant_ready_events_for(
        &self,
        participant: &ParticipantId,
    ) -> Result<ParticipantReadyEvents> {
        self.participant_ready_events_with_selector(ParticipantReadyKey::participant_selector(
            self.root(),
            participant,
        )?)
        .await
    }

    /// Observe one participant's Ready keys directly on the transport
    /// callback. This is for ingress fences that must linearize Ready loss
    /// with a sample before the participant's next step.
    pub async fn observe_participant_ready_for(
        &self,
        participant: &ParticipantId,
        callback: impl Fn(ParticipantReadyEvent) + Send + Sync + 'static,
    ) -> Result<ParticipantReadyObserver> {
        self.observe_participant_ready_selector(
            ParticipantReadyKey::participant_selector(self.root(), participant)?,
            callback,
        )
        .await
    }

    async fn participant_ready_events_with_selector(
        &self,
        selector: OwnedKeyExpr,
    ) -> Result<ParticipantReadyEvents> {
        let (sender, receiver) = mpsc::channel(64);
        let overflowed = Arc::new(AtomicBool::new(false));
        let dropped = Arc::clone(&overflowed);
        let observer = self
            .observe_participant_ready_selector(selector, move |event| {
                if sender.try_send(event).is_err() {
                    dropped.store(true, Ordering::Release);
                }
            })
            .await?;
        Ok(ParticipantReadyEvents {
            receiver: Mutex::new(receiver),
            _observer: observer,
            overflowed,
        })
    }

    /// Observe participant appearance and disappearance, including tokens that
    /// were already live when this observer was declared.
    ///
    /// The callback runs on Zenoh's runtime and must not perform Zenoh network
    /// operations. Sending the event to a local channel or updating local state
    /// is appropriate.
    ///
    /// Callers that render stable participant presence must aggregate the exact
    /// per-producer events and consider the participant present while at least
    /// one producer remains live.
    pub async fn observe_participant_ready(
        &self,
        callback: impl Fn(ParticipantReadyEvent) + Send + Sync + 'static,
    ) -> Result<ParticipantReadyObserver> {
        let root = self.root().to_string();
        let selector = ParticipantReadyKey::selector(&root)?;
        self.observe_participant_ready_selector(selector, callback)
            .await
    }

    /// Observe the exact selector used by a participant-scoped Ready stream.
    async fn observe_participant_ready_selector(
        &self,
        selector: OwnedKeyExpr,
        callback: impl Fn(ParticipantReadyEvent) + Send + Sync + 'static,
    ) -> Result<ParticipantReadyObserver> {
        let root = self.root().to_string();
        let subscriber = self
            .session()?
            .liveliness()
            .declare_subscriber(selector)
            .history(true)
            .callback(move |sample| {
                if let Some(event) =
                    participant_event(&root, sample.key_expr().as_str(), sample.kind())
                {
                    callback(event);
                }
            })
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        Ok(ParticipantReadyObserver {
            _subscriber: subscriber,
        })
    }

    /// Observe one exact Liveliness key below this session's root.
    ///
    /// `relative_key` is a concrete key path below the execution root, for
    /// example `supervisor/identity`; the root is this session's, so an
    /// observer can never accidentally watch another execution's token.
    ///
    /// The returned observer carries the token's current state
    /// ([`KeyLivelinessObserver::initial`]) and `callback` receives every
    /// change after that. Statuses are levels, not edges: the subscriber is
    /// declared before the state is read, so a token that appears during that
    /// window is reported twice - once through the callback and once as the
    /// initial state - and a consumer must be indifferent to that.
    ///
    /// Reading that initial state is a query, and a query can fail. A failed
    /// reply is surfaced as [`BusError::Transport`] rather than reported as
    /// [`LivelinessStatus::Lost`]: "the query broke" and "the token is not
    /// there" lead a client to opposite conclusions, so they are never the same
    /// value. No reply at all does mean absent, and stays `Lost`.
    ///
    /// The callback runs on Zenoh's runtime and must not perform Zenoh network
    /// operations.
    pub async fn observe_liveliness_key(
        &self,
        relative_key: &str,
        callback: impl Fn(LivelinessStatus) + Send + Sync + 'static,
    ) -> Result<KeyLivelinessObserver> {
        validate_relative_key(relative_key)?;
        let raw = self.full_key(relative_key);
        let key = OwnedKeyExpr::new(raw.clone())
            .map_err(|error| BusError::not_a_key_expression(&raw, error))?;

        // The initial liveliness query may wait for replies. Keep one
        // admission lease across declaration, query, and receive so close
        // cannot race the long-lived receive path.
        let session = self.session()?;

        // Declared before the state is read, so a token lost in between is
        // reported by the subscriber rather than missed by both.
        let subscriber = session
            .liveliness()
            .declare_subscriber(key.clone())
            .callback(move |sample| callback(LivelinessStatus::from(sample.kind())))
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;

        let replies = session
            .liveliness()
            .get(key)
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        let mut initial = LivelinessStatus::Lost;
        while let Ok(reply) = replies.recv_async().await {
            match reply.result() {
                Ok(_) => initial = LivelinessStatus::Alive,
                // An error reply is a failed query, not an answer of "absent".
                // Folding it into `Lost` would report a token as gone on the
                // strength of the query having broken, which is exactly the
                // reading a client uses to decide the robot is not there.
                // Silence - the channel closing with no reply at all - genuinely
                // does mean absent and stays `Lost`.
                Err(error) => {
                    return Err(BusError::Transport(format!(
                        "the liveliness query for '{raw}' failed: {error:?}"
                    )));
                }
            }
        }

        Ok(KeyLivelinessObserver {
            _subscriber: subscriber,
            initial,
        })
    }
}

/// A liveliness key must be a concrete path below the execution root: a
/// wildcard would silently widen the observation to whatever else exists.
fn validate_relative_key(relative_key: &str) -> Result<()> {
    validate_concrete_path(relative_key)
}

fn participant_event(root: &str, key: &str, kind: SampleKind) -> Option<ParticipantReadyEvent> {
    let key = ParticipantReadyKey::parse(root, key)?;
    Some(ParticipantReadyEvent {
        source: key.source().clone(),
        status: ParticipantReadyStatus::from(kind),
    })
}

/// A participant id becomes exactly one key segment, so it may carry neither a
/// separator nor a wildcard. The typed `ParticipantId` constructor applies its
/// stricter topology grammar after this transport-specific diagnosis.
fn validate_participant(participant: &str) -> Result<()> {
    if participant.is_empty() {
        return Err(BusError::invalid_key(participant, KeyProblem::Empty));
    }
    if participant.contains('/') {
        return Err(BusError::invalid_key(
            participant,
            KeyProblem::NotOneSegment,
        ));
    }
    if participant.contains('*') {
        return Err(BusError::invalid_key(participant, KeyProblem::Wildcard));
    }
    Ok(())
}

fn validate_root(root: &str) -> Result<()> {
    validate_concrete_path(root)
}

fn validate_concrete_path(path: &str) -> Result<()> {
    if path.is_empty() || path.split('/').any(str::is_empty) {
        return Err(BusError::invalid_key(path, KeyProblem::Empty));
    }
    if path.contains('*') {
        return Err(BusError::invalid_key(path, KeyProblem::Wildcard));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "phoxal/ffffffffffffffffffffffffffffffff";

    /// A distinct deterministic test producer. Production sessions mint their
    /// producer through the bus owner, while tests name theirs explicitly.
    fn producer(value: u128) -> ProducerId {
        ProducerId::try_from((1_u128 << 124) | value).expect("a test producer is canonical")
    }

    #[test]
    fn key_builder_owns_validation_and_round_trips_identity() {
        let producer = producer(1);
        let key = ParticipantReadyKey::new(ROOT, "drive", producer).unwrap();
        assert_eq!(
            key.as_str(),
            format!("{ROOT}/liveliness/participants/drive/{producer}")
        );
        assert_eq!(
            ParticipantReadyKey::parse(ROOT, key.as_str()),
            Some(key.clone())
        );
        assert_eq!(key.participant().as_str(), "drive");
        assert_eq!(key.producer(), producer);
        assert!(ParticipantReadyKey::new(ROOT, "bad/id", producer).is_err());
        assert!(ParticipantReadyKey::new("phoxal/*", "drive", producer).is_err());
        assert!(
            ParticipantReadyKey::parse(
                ROOT,
                &format!("{ROOT}/liveliness/participants/drive/not-a-producer")
            )
            .is_none()
        );
    }

    #[test]
    fn participant_event_maps_sample_kinds() {
        let producer = producer(2);
        let key = format!("{ROOT}/liveliness/participants/drive/{producer}");
        let alive = participant_event(ROOT, &key, SampleKind::Put).unwrap();
        let lost = participant_event(ROOT, &key, SampleKind::Delete).unwrap();

        assert_eq!(alive.participant().as_str(), "drive");
        assert_eq!(alive.producer(), producer);
        assert_eq!(alive.status, ParticipantReadyStatus::Ready);
        assert_eq!(lost.status, ParticipantReadyStatus::Lost);
        assert!(participant_event(ROOT, "other/robot/key", SampleKind::Put).is_none());
    }

    #[test]
    fn observer_selector_covers_exactly_the_emitted_identity_segments() {
        let root = ROOT;
        let selector = ParticipantReadyKey::selector(root).unwrap();
        let key = ParticipantReadyKey::new(root, "drive", producer(3)).unwrap();
        assert_eq!(
            selector.as_str(),
            format!("{ROOT}/liveliness/participants/*/*")
        );
        assert!(selector.includes(&key.key));
    }

    #[test]
    fn participant_scoped_selector_excludes_unrelated_ready_churn() {
        let root = ROOT;
        let expected = ParticipantId::new("safety").unwrap();
        let selected = ParticipantReadyKey::participant_selector(root, &expected).unwrap();
        let safety = ParticipantReadyKey::new(root, "safety", producer(4)).unwrap();
        let navigation = ParticipantReadyKey::new(root, "navigation", producer(5)).unwrap();
        assert_eq!(
            selected.as_str(),
            format!("{root}/liveliness/participants/safety/*")
        );
        assert!(selected.includes(&safety.key));
        assert!(!selected.includes(&navigation.key));
    }

    /// The supervisor's presence token is one exact key, and a client attaching
    /// mid-run has to learn both that it is already there and, later, that it is
    /// gone. Neither half is answerable from the participant-scoped observer.
    ///
    /// These need two separate sessions to reach each other, so they run behind
    /// the `router` feature: an in-process session cannot observe another one.
    #[cfg(feature = "router")]
    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_exact_key_observation_reports_a_token_that_was_already_live_and_then_its_loss() {
        use std::sync::Arc;
        use std::sync::Mutex;
        use std::time::Duration;

        use crate::session::BusConfig;
        use crate::test_support::socket_endpoint;

        let (_dir, endpoint) = socket_endpoint("phoxal-presence-");
        let execution = phoxal_runtime_contract::identity::ExecutionId::mint();
        let router = crate::router::Router::open(execution, std::slice::from_ref(&endpoint), None)
            .await
            .expect("the router binds its endpoint");

        let session = |participant: &str| {
            BusConfig::for_participant(
                execution,
                phoxal_runtime_contract::identity::ParticipantId::new(participant)
                    .expect("test participant id"),
                vec![endpoint.clone()],
            )
        };

        // The declaring side goes first, so the observer genuinely attaches late.
        let (declaring_owner, _declaring) = BusOwner::open(session("supervisor")).await.unwrap();
        let token = declaring_owner
            .declare_liveliness_key("supervisor/presence")
            .await
            .expect("the supervisor declares its presence token");
        assert_eq!(token.relative_key(), "supervisor/presence");
        assert!(
            declaring_owner
                .declare_liveliness_key("liveliness/participants/drive/producer")
                .await
                .is_err(),
            "infrastructure leases cannot mint participant Ready authority"
        );
        tokio::time::sleep(Duration::from_millis(200)).await;

        let (observing_owner, observing) = BusOwner::open(session("client")).await.unwrap();
        let seen = Arc::new(Mutex::new(Vec::new()));
        let recorder = Arc::clone(&seen);
        let observer = observing
            .observe_liveliness_key("supervisor/presence", move |status| {
                crate::lock::lock(&recorder).push(status);
            })
            .await
            .expect("the exact key is observable");
        assert_eq!(
            observer.initial(),
            LivelinessStatus::Alive,
            "a token declared before the observer must be reported as present"
        );

        drop(token);

        let mut lost = false;
        for _ in 0..100 {
            if crate::lock::lock(&seen).contains(&LivelinessStatus::Lost) {
                lost = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(lost, "undeclaring the token must be reported as loss");

        drop(observer);
        observing_owner.close().await;
        declaring_owner.close().await;
        router.close().await.unwrap();
    }

    /// An observation that reported presence for a token that was never there
    /// would make an attach succeed against a dead robot.
    #[cfg(feature = "router")]
    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn an_exact_key_observation_reports_an_absent_token_as_lost() {
        use crate::session::BusConfig;
        use crate::test_support::socket_endpoint;

        let (_dir, endpoint) = socket_endpoint("phoxal-presence-absent-");
        let execution = phoxal_runtime_contract::identity::ExecutionId::mint();
        let router = crate::router::Router::open(execution, std::slice::from_ref(&endpoint), None)
            .await
            .expect("the router binds its endpoint");

        let (observing_owner, observing) = BusOwner::open(BusConfig::for_participant(
            execution,
            phoxal_runtime_contract::identity::ParticipantId::new("client")
                .expect("test participant id"),
            vec![endpoint.clone()],
        ))
        .await
        .unwrap();
        let observer = observing
            .observe_liveliness_key("supervisor/presence", |_| {})
            .await
            .expect("an absent token is still an observable key");
        assert_eq!(observer.initial(), LivelinessStatus::Lost);

        // A wildcard would silently widen the observation to whatever else exists.
        assert!(
            observing
                .observe_liveliness_key("supervisor/*", |_| {})
                .await
                .is_err(),
            "an exact-key observation must reject a selector"
        );

        drop(observer);
        observing_owner.close().await;
        router.close().await.unwrap();
    }

    /// The observer is the subscription: dropping it has to stop delivery, or a
    /// client that tore down its attach would keep mutating state behind a screen
    /// it no longer shows. Delivery is proven live first, so the silence afterwards
    /// is the drop taking effect and not a setup that never worked.
    #[cfg(feature = "router")]
    #[serial_test::serial]
    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn dropping_an_exact_key_observer_stops_callback_delivery() {
        use std::sync::Arc;
        use std::sync::atomic::{AtomicUsize, Ordering};
        use std::time::Duration;

        use crate::session::BusConfig;
        use crate::test_support::socket_endpoint;

        let (_dir, endpoint) = socket_endpoint("phoxal-identity-drop-");
        let execution = phoxal_runtime_contract::identity::ExecutionId::mint();
        let router = crate::router::Router::open(execution, std::slice::from_ref(&endpoint), None)
            .await
            .expect("the router binds its endpoint");

        let session = |participant: &str| {
            BusConfig::for_participant(
                execution,
                phoxal_runtime_contract::identity::ParticipantId::new(participant)
                    .expect("test participant id"),
                vec![endpoint.clone()],
            )
        };
        let (declaring_owner, _declaring) = BusOwner::open(session("supervisor")).await.unwrap();
        let (observing_owner, observing) = BusOwner::open(session("client")).await.unwrap();

        let calls = Arc::new(AtomicUsize::new(0));
        let counter = Arc::clone(&calls);
        let observer = observing
            .observe_liveliness_key("supervisor/presence", move |_| {
                counter.fetch_add(1, Ordering::Relaxed);
            })
            .await
            .expect("the exact key is observable");
        assert_eq!(observer.initial(), LivelinessStatus::Lost);

        // Prove the subscription is live: a first declaration must reach it.
        let token = declaring_owner
            .declare_liveliness_key("supervisor/presence")
            .await
            .expect("the supervisor declares its presence token");
        let mut delivered = false;
        for _ in 0..100 {
            if calls.load(Ordering::Relaxed) > 0 {
                delivered = true;
                break;
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        assert!(delivered, "a live observer must receive the declaration");
        let before_drop = calls.load(Ordering::Relaxed);

        drop(observer);
        // The undeclare that follows would be delivered to a still-declared
        // subscriber, so any increment here is the observer having outlived its
        // handle.
        drop(token);
        tokio::time::sleep(Duration::from_millis(500)).await;
        assert_eq!(
            calls.load(Ordering::Relaxed),
            before_drop,
            "a dropped observer must not receive further changes"
        );

        observing_owner.close().await;
        declaring_owner.close().await;
        router.close().await.unwrap();
    }
}
