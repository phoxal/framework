//! Participant presence built on Zenoh Liveliness.
//!
//! Each token is keyed by robot, participant id, and process incarnation.
//! Observers receive exact incarnation events and can aggregate them by the
//! stable participant id for present/not-present UI state.

use zenoh::key_expr::OwnedKeyExpr;
use zenoh::sample::SampleKind;

use crate::{Bus, BusError, Result};

const PARTICIPANT_LIVELINESS_PREFIX: &str = "liveliness/participants";

/// One stable participant Liveliness key.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParticipantLivelinessKey {
    key: OwnedKeyExpr,
    participant: String,
    incarnation: u64,
}

impl ParticipantLivelinessKey {
    pub(crate) fn for_bus(bus: &Bus) -> Result<Self> {
        Self::new(bus.root(), bus.participant(), bus.incarnation())
    }

    /// Build and validate an incarnation-qualified participant key below an
    /// existing robot root.
    pub fn new(robot_root: &str, participant: impl Into<String>, incarnation: u64) -> Result<Self> {
        validate_robot_root(robot_root)?;
        let participant = participant.into();
        validate_participant(&participant)?;
        let raw =
            format!("{robot_root}/{PARTICIPANT_LIVELINESS_PREFIX}/{participant}/{incarnation}");
        let key = OwnedKeyExpr::new(raw.clone()).map_err(|error| {
            BusError::Namespace(format!("invalid Liveliness key '{raw}': {error}"))
        })?;
        Ok(Self {
            key,
            participant,
            incarnation,
        })
    }

    /// The complete robot-rooted Zenoh key.
    pub fn as_str(&self) -> &str {
        self.key.as_str()
    }

    /// Participant id encoded in the key.
    pub fn participant(&self) -> &str {
        &self.participant
    }

    /// Per-process incarnation encoded in the key.
    pub fn incarnation(&self) -> u64 {
        self.incarnation
    }

    /// Parse a concrete participant key emitted below `robot_root`.
    pub fn parse(robot_root: &str, key: &str) -> Option<Self> {
        let suffix = key.strip_prefix(robot_root)?.strip_prefix('/')?;
        let suffix = suffix
            .strip_prefix(PARTICIPANT_LIVELINESS_PREFIX)?
            .strip_prefix('/')?;
        let (participant, incarnation) = suffix.split_once('/')?;
        if incarnation.contains('/') {
            return None;
        }
        Self::new(robot_root, participant, incarnation.parse().ok()?).ok()
    }

    /// Wildcard selector used by a robot-scoped observer.
    pub fn selector(robot_root: &str) -> Result<OwnedKeyExpr> {
        validate_robot_root(robot_root)?;
        let selector = format!("{robot_root}/{PARTICIPANT_LIVELINESS_PREFIX}/*/*");
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
    /// incarnation events and consider the participant present while at least
    /// one incarnation remains live.
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
    robot_root: &str,
    key: &str,
    kind: SampleKind,
) -> Option<ParticipantLivelinessEvent> {
    let key = ParticipantLivelinessKey::parse(robot_root, key)?;
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

fn validate_robot_root(root: &str) -> Result<()> {
    if root.is_empty()
        || root
            .split('/')
            .any(|segment| segment.is_empty() || segment.contains('*'))
    {
        return Err(BusError::Namespace(format!(
            "robot root must be a concrete non-empty key path, got '{root}'"
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn key_builder_owns_validation_and_round_trips_identity() {
        let key = ParticipantLivelinessKey::new("dev/robots/rover", "drive", 42).unwrap();
        assert_eq!(
            key.as_str(),
            "dev/robots/rover/liveliness/participants/drive/42"
        );
        assert_eq!(
            ParticipantLivelinessKey::parse("dev/robots/rover", key.as_str()),
            Some(key.clone())
        );
        assert_eq!(key.participant(), "drive");
        assert_eq!(key.incarnation(), 42);
        assert!(ParticipantLivelinessKey::new("dev/robots/rover", "bad/id", 42).is_err());
        assert!(ParticipantLivelinessKey::new("dev/*", "drive", 42).is_err());
        assert!(
            ParticipantLivelinessKey::parse(
                "dev/robots/rover",
                "dev/robots/rover/liveliness/participants/drive/not-a-number"
            )
            .is_none()
        );
    }

    #[test]
    fn participant_event_maps_sample_kinds() {
        let root = "dev/robots/rover";
        let key = "dev/robots/rover/liveliness/participants/drive/7";
        let alive = participant_event(root, key, SampleKind::Put).unwrap();
        let lost = participant_event(root, key, SampleKind::Delete).unwrap();

        assert_eq!(alive.key.participant(), "drive");
        assert_eq!(alive.key.incarnation(), 7);
        assert_eq!(alive.status, ParticipantLivelinessStatus::Alive);
        assert_eq!(lost.status, ParticipantLivelinessStatus::Lost);
        assert!(participant_event(root, "other/robot/key", SampleKind::Put).is_none());
    }

    #[test]
    fn observer_selector_covers_exactly_the_emitted_identity_segments() {
        let root = "dev/robots/rover";
        let selector = ParticipantLivelinessKey::selector(root).unwrap();
        let key = ParticipantLivelinessKey::new(root, "drive", 9).unwrap();
        assert_eq!(
            selector.as_str(),
            "dev/robots/rover/liveliness/participants/*/*"
        );
        assert!(selector.includes(&key.key));
    }
}
