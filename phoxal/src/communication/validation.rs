use std::collections::BTreeSet;

use super::bootstrap::{SessionOffer, SessionOffers};

/// The only public session protocol implemented by the first cutover.
pub const SESSION_PROTOCOL: &str = "phoxal.session.v1";
/// Maximum encoded bootstrap response size.
pub const MAX_BOOTSTRAP_BYTES: usize = 16 * 1024;
/// Maximum number of offered public session protocols.
pub const MAX_SESSION_OFFERS: usize = 16;
/// Maximum protocol identifier length in ASCII bytes.
pub const MAX_PROTOCOL_BYTES: usize = 128;
/// Maximum concrete session key prefix length in ASCII bytes.
pub const MAX_KEY_PREFIX_BYTES: usize = 512;

/// Explicit public deployment namespace selected by a client or supervisor.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct DeploymentTarget {
    scope: String,
    supervisor: String,
}

impl DeploymentTarget {
    /// Validate a scope and supervisor pair.
    ///
    /// # Errors
    ///
    /// Returns [`BootstrapError::InvalidTarget`] for an empty, oversized, or
    /// non-ASCII identifier.
    pub fn new(
        scope: impl Into<String>,
        supervisor: impl Into<String>,
    ) -> Result<Self, BootstrapError> {
        let scope = scope.into();
        let supervisor = supervisor.into();
        if !valid_identifier(&scope) || !valid_identifier(&supervisor) {
            return Err(BootstrapError::InvalidTarget);
        }
        Ok(Self { scope, supervisor })
    }

    /// Configured deployment scope.
    #[must_use]
    pub fn scope(&self) -> &str {
        &self.scope
    }

    /// Stable supervisor identifier within the scope.
    #[must_use]
    pub fn supervisor(&self) -> &str {
        &self.supervisor
    }

    /// Exact deployment prefix.
    #[must_use]
    pub fn prefix(&self) -> String {
        format!("phoxal/{}/supervisors/{}", self.scope, self.supervisor)
    }

    /// Exact offer-only query key.
    #[must_use]
    pub fn bootstrap_key(&self) -> String {
        format!("{}/bootstrap/v1", self.prefix())
    }

    /// Exact v1 public session prefix.
    #[must_use]
    pub fn session_prefix(&self) -> String {
        format!("{}/session/v1", self.prefix())
    }
}

/// Validate and select the first locally supported offer.
///
/// Unknown well-formed offers are ignored.
/// The encoded bound must be checked before decoding and is repeated here so
/// callers cannot accidentally validate an already oversized response.
///
/// # Errors
///
/// Returns a typed error for malformed, duplicate, redirected, oversized, or
/// unsupported offers.
pub fn validate_session_offers(
    target: &DeploymentTarget,
    encoded_len: usize,
    offers: &SessionOffers,
) -> Result<SessionOffer, BootstrapError> {
    if encoded_len > MAX_BOOTSTRAP_BYTES {
        return Err(BootstrapError::ResponseTooLarge);
    }
    if offers.sessions.len() > MAX_SESSION_OFFERS {
        return Err(BootstrapError::TooManyOffers);
    }
    let mut protocols = BTreeSet::new();
    let mut selected = None;
    for offer in &offers.sessions {
        if !valid_protocol(&offer.protocol) {
            return Err(BootstrapError::InvalidProtocol);
        }
        if !protocols.insert(offer.protocol.as_str()) {
            return Err(BootstrapError::DuplicateProtocol);
        }
        if !valid_key_prefix(&offer.key_prefix) {
            return Err(BootstrapError::InvalidKeyPrefix);
        }
        if offer.protocol == SESSION_PROTOCOL {
            if offer.key_prefix != target.session_prefix() {
                return Err(BootstrapError::RedirectedOffer);
            }
            selected = Some(offer.clone());
        }
    }
    selected.ok_or(BootstrapError::UnsupportedSessionProtocol)
}

pub fn valid_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.is_ascii()
        && value
            .bytes()
            .next()
            .is_some_and(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        && value.bytes().skip(1).all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}

fn valid_protocol(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_PROTOCOL_BYTES
        && value.is_ascii()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.'))
}

fn valid_key_prefix(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_KEY_PREFIX_BYTES
        && value.is_ascii()
        && !value.contains('*')
        && !value.contains('?')
        && !value.starts_with('/')
        && !value.ends_with('/')
        && value.split('/').all(|segment| !segment.is_empty())
}

/// Bootstrap validation failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum BootstrapError {
    /// The selected deployment namespace is invalid.
    #[error("scope and supervisor identifiers must be bounded ASCII identifiers")]
    InvalidTarget,
    /// The encoded response exceeds the fixed bootstrap bound.
    #[error("bootstrap response exceeds 16 KiB")]
    ResponseTooLarge,
    /// More than 16 offers were returned.
    #[error("bootstrap response contains more than 16 offers")]
    TooManyOffers,
    /// A protocol identifier is malformed.
    #[error("bootstrap protocol identifier is malformed")]
    InvalidProtocol,
    /// One protocol was offered more than once.
    #[error("bootstrap response contains a duplicate protocol")]
    DuplicateProtocol,
    /// An offer contains an invalid concrete key prefix.
    #[error("bootstrap session key prefix is malformed")]
    InvalidKeyPrefix,
    /// A known protocol points outside its fixed deployment prefix.
    #[error("bootstrap offer redirects the known session protocol")]
    RedirectedOffer,
    /// No locally implemented session protocol was offered.
    #[error("supervisor offers no supported public session protocol")]
    UnsupportedSessionProtocol,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> DeploymentTarget {
        DeploymentTarget::new("workshop", "rover-01").expect("valid target")
    }

    fn offer(protocol: &str, prefix: &str) -> SessionOffer {
        SessionOffer {
            protocol: protocol.to_owned(),
            key_prefix: prefix.to_owned(),
        }
    }

    #[test]
    fn fixed_keys_and_the_single_supported_offer_validate() {
        let target = target();
        assert_eq!(
            target.bootstrap_key(),
            "phoxal/workshop/supervisors/rover-01/bootstrap/v1"
        );
        let offers = SessionOffers {
            sessions: vec![offer(SESSION_PROTOCOL, &target.session_prefix())],
        };
        let selected = validate_session_offers(&target, 64, &offers).expect("supported offer");
        assert_eq!(selected.protocol, SESSION_PROTOCOL);
    }

    #[test]
    fn unknown_offers_are_ignored_without_becoming_v1() {
        let target = target();
        let offers = SessionOffers {
            sessions: vec![offer("phoxal.session.v2", &target.session_prefix())],
        };
        assert_eq!(
            validate_session_offers(&target, 64, &offers),
            Err(BootstrapError::UnsupportedSessionProtocol)
        );
    }

    #[test]
    fn duplicate_redirected_and_oversized_offers_are_refused() {
        let target = target();
        let duplicate = SessionOffers {
            sessions: vec![
                offer(SESSION_PROTOCOL, &target.session_prefix()),
                offer(SESSION_PROTOCOL, &target.session_prefix()),
            ],
        };
        assert_eq!(
            validate_session_offers(&target, 128, &duplicate),
            Err(BootstrapError::DuplicateProtocol)
        );
        let redirected = SessionOffers {
            sessions: vec![offer(
                SESSION_PROTOCOL,
                "phoxal/other/supervisors/rover-01/session/v1",
            )],
        };
        assert_eq!(
            validate_session_offers(&target, 128, &redirected),
            Err(BootstrapError::RedirectedOffer)
        );
        assert_eq!(
            validate_session_offers(&target, MAX_BOOTSTRAP_BYTES + 1, &SessionOffers::default()),
            Err(BootstrapError::ResponseTooLarge)
        );
    }

    #[test]
    fn deployment_segments_use_the_fixed_routing_grammar() {
        for invalid in ["", "Workshop", "-rover", "rover.1", &"r".repeat(65)] {
            assert_eq!(
                DeploymentTarget::new(invalid, "rover-01"),
                Err(BootstrapError::InvalidTarget)
            );
        }
        let target = target();
        assert_eq!(target.scope(), "workshop");
        assert_eq!(target.supervisor(), "rover-01");
    }
}
