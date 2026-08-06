//! Participant presence built on Zenoh Liveliness.
//!
//! Each token is keyed by execution root, participant id, and producer
//! identity. Observers receive exact per-producer events and can aggregate them
//! by the stable participant id for present/not-present UI state. Because the
//! root is execution-scoped, a previous run's tokens are on different keys
//! entirely and can never be mistaken for the current run's.

use zenoh::key_expr::OwnedKeyExpr;
use zenoh::sample::SampleKind;

use crate::identity::ProducerId;
use crate::{Bus, BusError, Result};

const PARTICIPANT_LIVELINESS_PREFIX: &str = "liveliness/participants";

/// One stable participant Liveliness key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParticipantLivelinessKey {
    key: OwnedKeyExpr,
    participant: String,
    producer: ProducerId,
}

impl ParticipantLivelinessKey {
    pub(crate) fn for_bus(bus: &Bus) -> Result<Self> {
        Self::new(bus.root(), bus.participant(), bus.producer())
    }

    /// Build and validate a producer-qualified participant key below an
    /// existing execution root.
    pub fn new(root: &str, participant: impl Into<String>, producer: ProducerId) -> Result<Self> {
        validate_root(root)?;
        let participant = participant.into();
        validate_participant(&participant)?;
        let raw = format!("{root}/{PARTICIPANT_LIVELINESS_PREFIX}/{participant}/{producer}");
        let key = OwnedKeyExpr::new(raw.clone()).map_err(|error| {
            BusError::Namespace(format!("invalid Liveliness key '{raw}': {error}"))
        })?;
        Ok(Self {
            key,
            participant,
            producer,
        })
    }

    /// The complete execution-rooted Zenoh key.
    pub fn as_str(&self) -> &str {
        self.key.as_str()
    }

    /// Participant id encoded in the key.
    pub fn participant(&self) -> &str {
        &self.participant
    }

    /// Producer identity encoded in the key.
    pub fn producer(&self) -> ProducerId {
        self.producer
    }

    /// Parse a concrete participant key emitted below `root`.
    pub fn parse(root: &str, key: &str) -> Option<Self> {
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
    pub fn selector(root: &str) -> Result<OwnedKeyExpr> {
        validate_root(root)?;
        let selector = format!("{root}/{PARTICIPANT_LIVELINESS_PREFIX}/*/*");
        OwnedKeyExpr::new(selector.clone()).map_err(|error| {
            BusError::Namespace(format!("invalid Liveliness selector '{selector}': {error}"))
        })
    }
}

/// Appearance or disappearance of one participant.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ParticipantLivelinessStatus {
    Alive,
    Lost,
}

/// A parsed participant Liveliness observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParticipantLivelinessEvent {
    pub key: ParticipantLivelinessKey,
    pub status: ParticipantLivelinessStatus,
}

/// Keeps a declared participant token alive until it is dropped.
pub struct ParticipantLivelinessToken {
    _token: zenoh::liveliness::LivelinessToken,
    key: ParticipantLivelinessKey,
}

impl ParticipantLivelinessToken {
    /// The concrete key represented by this token.
    pub fn key(&self) -> &ParticipantLivelinessKey {
        &self.key
    }
}

/// Keeps a history-enabled participant observer declared until it is dropped.
pub struct ParticipantLivelinessObserver {
    _subscriber: zenoh::pubsub::Subscriber<()>,
}

impl Bus {
    /// Declare this bus participant's token. Call this only after setup succeeds.
    pub async fn declare_participant_liveliness(&self) -> Result<ParticipantLivelinessToken> {
        let key = ParticipantLivelinessKey::for_bus(self)?;
        let token = self
            .session()
            .liveliness()
            .declare_token(key.as_str())
            .await
            .map_err(|error| BusError::Transport(error.to_string()))?;
        Ok(ParticipantLivelinessToken { _token: token, key })
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
    pub async fn observe_participant_liveliness(
        &self,
        callback: impl Fn(ParticipantLivelinessEvent) + Send + Sync + 'static,
    ) -> Result<ParticipantLivelinessObserver> {
        let root = self.root().to_string();
        let selector = ParticipantLivelinessKey::selector(&root)?;
        let subscriber = self
            .session()
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
        Ok(ParticipantLivelinessObserver {
            _subscriber: subscriber,
        })
    }
}

fn participant_event(
    root: &str,
    key: &str,
    kind: SampleKind,
) -> Option<ParticipantLivelinessEvent> {
    let key = ParticipantLivelinessKey::parse(root, key)?;
    let status = match kind {
        SampleKind::Put => ParticipantLivelinessStatus::Alive,
        SampleKind::Delete => ParticipantLivelinessStatus::Lost,
    };
    Some(ParticipantLivelinessEvent { key, status })
}

fn validate_participant(participant: &str) -> Result<()> {
    if participant.is_empty() {
        return Err(BusError::Namespace(
            "participant id must not be empty".to_string(),
        ));
    }
    if participant.contains('/') || participant.contains('*') {
        return Err(BusError::Namespace(format!(
            "participant id must be one concrete key segment, got '{participant}'"
        )));
    }
    Ok(())
}

fn validate_root(root: &str) -> Result<()> {
    if root.is_empty()
        || root
            .split('/')
            .any(|segment| segment.is_empty() || segment.contains('*'))
    {
        return Err(BusError::Namespace(format!(
            "an execution root must be a concrete non-empty key path, got '{root}'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    const ROOT: &str = "phoxal/ffffffffffffffffffffffffffffffff";

    /// A distinct test producer. Nothing mints a producer in production - a
    /// session's identity is the session - so tests name theirs explicitly.
    fn producer(value: u128) -> ProducerId {
        ProducerId::try_from(value).expect("a test producer is nonzero")
    }

    #[test]
    fn key_builder_owns_validation_and_round_trips_identity() {
        let producer = producer(1);
        let key = ParticipantLivelinessKey::new(ROOT, "drive", producer).unwrap();
        assert_eq!(
            key.as_str(),
            format!("{ROOT}/liveliness/participants/drive/{producer}")
        );
        assert_eq!(
            ParticipantLivelinessKey::parse(ROOT, key.as_str()),
            Some(key.clone())
        );
        assert_eq!(key.participant(), "drive");
        assert_eq!(key.producer(), producer);
        assert!(ParticipantLivelinessKey::new(ROOT, "bad/id", producer).is_err());
        assert!(ParticipantLivelinessKey::new("phoxal/*", "drive", producer).is_err());
        assert!(
            ParticipantLivelinessKey::parse(
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

        assert_eq!(alive.key.participant(), "drive");
        assert_eq!(alive.key.producer(), producer);
        assert_eq!(alive.status, ParticipantLivelinessStatus::Alive);
        assert_eq!(lost.status, ParticipantLivelinessStatus::Lost);
        assert!(participant_event(ROOT, "other/robot/key", SampleKind::Put).is_none());
    }

    #[test]
    fn observer_selector_covers_exactly_the_emitted_identity_segments() {
        let root = ROOT;
        let selector = ParticipantLivelinessKey::selector(root).unwrap();
        let key = ParticipantLivelinessKey::new(root, "drive", producer(3)).unwrap();
        assert_eq!(
            selector.as_str(),
            format!("{ROOT}/liveliness/participants/*/*")
        );
        assert!(selector.includes(&key.key));
    }
}
