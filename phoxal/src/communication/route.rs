//! Shared public-session route grammar.
//!
//! [`PublicRouteKind`], [`PublicOperation`], and [`PublicRoute`] describe what a
//! legal public route is: the protected ACL lane, the exact operation suffix,
//! and the parsed key shape under a validated [`DeploymentTarget`]. They are
//! shared contract vocabulary — clients use them to construct and validate
//! routes before any transport is opened, and the supervisor uses the same
//! vocabulary to authorise or refuse an incoming key.
//!
//! This module owns no mutable server state. The supervisor admission logic
//! lives in `supervisor_adapter`; here only the pure grammar and parsing
//! helpers are kept.

use super::bootstrap::{SessionOffer, SessionOffers};
use super::validation::{DeploymentTarget, SESSION_PROTOCOL, valid_identifier};

// `SupervisorAdapterError` is the supervisor's error type; the route grammar
// only needs to return a typed error, so we model it locally to keep the SDK
// free of supervisor state. The supervisor implements `From<RouteError> for
// SupervisorAdapterError` if it ever needs to convert.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RouteError {
    #[error("route principal is not a valid protected route segment")]
    InvalidPrincipal,
    #[error("route kind does not match the requested operation")]
    RouteKindMismatch {
        expected: super::PublicRouteKind,
        actual: super::PublicRouteKind,
    },
    #[error("route is not an exact public session route")]
    MalformedRoute,
    #[error("key does not address the supplied supervisor")]
    WrongRoute,
}

/// The protected public route class used by a session request.
///
/// The router should authorize these lanes separately. The adapter checks the
/// same class after the route has reached the supervisor, so a command cannot
/// be tunneled through an inspection route by changing only a request body.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PublicRouteKind {
    /// Open, renew, and close control sessions.
    Control,
    /// Read-only metadata, lifecycle, execution, and binding admission.
    Inspection,
    /// Service operation admission for already bound mutable ports.
    Mutation,
    /// Deterministic simulation authority and boundary coordination.
    Simulation,
}

/// The exact operation suffix carried by one public session route.
///
/// Operation names are part of the routed contract, rather than inferred from
/// a request body. This lets router policy separate inspection from mutation
/// and lets the supervisor reject a request that was sent through the wrong
/// lane before it reaches domain admission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub enum PublicOperation {
    /// Establish a logical session.
    Open,
    /// Renew an established logical session.
    Renew,
    /// Close an established logical session.
    Close,
    /// Read executable and framework version information.
    Info,
    /// Read the current supervisor lifecycle projection.
    Status,
    /// List bounded execution summaries.
    ListExecutions,
    /// List bounded generated port metadata.
    ListPorts,
    /// Admit one exact generated public port binding.
    Bind,
    /// Read one bound public port.
    Read,
    /// Admit one request on a bound mutable public port.
    Command,
    /// Establish a bounded state/event observation.
    Watch,
    /// Establish a bounded event/stream subscription.
    Subscribe,
    /// Acquire exclusive simulation authority for one execution.
    AcquireAuthority,
    /// Admit the complete boundary-zero observation cut.
    AdmitInitialObservations,
    /// Prepare one admitted simulation boundary and select its actuator cut.
    PrepareBoundary,
    /// Admit the complete observation cut produced by native integration.
    AdmitObservations,
    /// Reset an acquired simulation timeline.
    Reset,
    /// Release simulation authority.
    ReleaseAuthority,
    /// Inspect the current simulation boundary.
    Progress,
}

impl PublicOperation {
    /// The exact key segment used for this operation.
    #[must_use]
    pub const fn segment(self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Renew => "renew",
            Self::Close => "close",
            Self::Info => "info",
            Self::Status => "status",
            Self::ListExecutions => "list-executions",
            Self::ListPorts => "list-ports",
            Self::Bind => "bind",
            Self::Read => "read",
            Self::Command => "command",
            Self::Watch => "watch",
            Self::Subscribe => "subscribe",
            Self::AcquireAuthority => "acquire-authority",
            Self::AdmitInitialObservations => "admit-initial-observations",
            Self::PrepareBoundary => "prepare-boundary",
            Self::AdmitObservations => "admit-observations",
            Self::Reset => "reset",
            Self::ReleaseAuthority => "release-authority",
            Self::Progress => "progress",
        }
    }

    fn from_segment(segment: &str) -> Option<Self> {
        Some(match segment {
            "open" => Self::Open,
            "renew" => Self::Renew,
            "close" => Self::Close,
            "info" => Self::Info,
            "status" => Self::Status,
            "list-executions" => Self::ListExecutions,
            "list-ports" => Self::ListPorts,
            "bind" => Self::Bind,
            "read" => Self::Read,
            "command" => Self::Command,
            "watch" => Self::Watch,
            "subscribe" => Self::Subscribe,
            "acquire-authority" => Self::AcquireAuthority,
            "admit-initial-observations" => Self::AdmitInitialObservations,
            "prepare-boundary" => Self::PrepareBoundary,
            "admit-observations" => Self::AdmitObservations,
            "reset" => Self::Reset,
            "release-authority" => Self::ReleaseAuthority,
            "progress" => Self::Progress,
            _ => return None,
        })
    }

    /// The ACL lane required for this operation.
    #[must_use]
    pub const fn kind(self) -> PublicRouteKind {
        match self {
            Self::Open | Self::Renew | Self::Close => PublicRouteKind::Control,
            Self::Info
            | Self::Status
            | Self::ListExecutions
            | Self::ListPorts
            | Self::Bind
            | Self::Read
            | Self::Watch
            | Self::Subscribe => PublicRouteKind::Inspection,
            Self::Command => PublicRouteKind::Mutation,
            Self::AcquireAuthority
            | Self::AdmitInitialObservations
            | Self::PrepareBoundary
            | Self::AdmitObservations
            | Self::Reset
            | Self::ReleaseAuthority
            | Self::Progress => PublicRouteKind::Simulation,
        }
    }
}

const fn default_operation(kind: PublicRouteKind) -> PublicOperation {
    match kind {
        PublicRouteKind::Control => PublicOperation::Open,
        PublicRouteKind::Inspection => PublicOperation::Info,
        PublicRouteKind::Mutation => PublicOperation::Command,
        PublicRouteKind::Simulation => PublicOperation::AcquireAuthority,
    }
}

impl PublicRouteKind {
    const fn segment(self) -> &'static str {
        match self {
            Self::Control => "control",
            Self::Inspection => "inspection",
            Self::Mutation => "mutation",
            Self::Simulation => "simulation",
        }
    }

    fn from_segment(segment: &str) -> Option<Self> {
        match segment {
            "control" => Some(Self::Control),
            "inspection" => Some(Self::Inspection),
            "mutation" => Some(Self::Mutation),
            "simulation" => Some(Self::Simulation),
            _ => None,
        }
    }
}

/// One exact public session route under a validated deployment target.
///
/// A route is the source of the asserted principal after trusted ingress has
/// protected the namespace. No request body field can override it. Construct
/// routes through [`DeploymentTarget::public_route`] or parse an exact
/// incoming key with [`PublicRoute::parse`].
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct PublicRoute {
    target: DeploymentTarget,
    principal: String,
    kind: PublicRouteKind,
    operation: PublicOperation,
}

impl PublicRoute {
    /// Construct one route for a validated principal and lane.
    ///
    /// # Errors
    ///
    /// Returns [`RouteError::InvalidPrincipal`] when the principal
    /// is not a valid protected route segment.
    pub fn new(
        target: &DeploymentTarget,
        principal: impl Into<String>,
        kind: PublicRouteKind,
    ) -> Result<Self, RouteError> {
        let principal = principal.into();
        if !valid_identifier(&principal) {
            return Err(RouteError::InvalidPrincipal);
        }
        Ok(Self {
            target: target.clone(),
            principal,
            kind,
            operation: default_operation(kind),
        })
    }

    /// Construct one principal-bound route for an exact operation.
    ///
    /// The operation determines its ACL lane. A caller cannot construct a
    /// command operation on an inspection route or vice versa.
    pub fn for_operation(
        target: &DeploymentTarget,
        principal: impl Into<String>,
        operation: PublicOperation,
    ) -> Result<Self, RouteError> {
        let route = Self::new(target, principal, operation.kind())?;
        Ok(Self { operation, ..route })
    }

    /// Return this route with an explicitly selected operation.
    pub fn with_operation(self, operation: PublicOperation) -> Result<Self, RouteError> {
        if operation.kind() != self.kind {
            return Err(RouteError::RouteKindMismatch {
                expected: operation.kind(),
                actual: self.kind,
            });
        }
        Ok(Self { operation, ..self })
    }

    /// Parse an exact public route key for the selected target.
    ///
    /// Accepted keys have exactly this shape:
    /// Session keys have the shape
    /// `{P}/session/v1/clients/{principal}/{control|inspection|mutation}/{operation}`.
    /// Simulation keys have the shape
    /// `{P}/simulation/v1/clients/{principal}/{operation}`.
    /// Wildcards, query expressions, extra path segments, redirects, and
    /// another supervisor's prefix are rejected.
    pub fn parse(target: &DeploymentTarget, key: &str) -> Result<Self, RouteError> {
        let session_prefix = format!("{}/clients/", target.session_prefix());
        if let Some(suffix) = key.strip_prefix(&session_prefix) {
            let mut segments = suffix.split('/');
            let principal = segments.next().ok_or(RouteError::MalformedRoute)?;
            let kind = segments
                .next()
                .and_then(PublicRouteKind::from_segment)
                .ok_or(RouteError::MalformedRoute)?;
            let operation = segments
                .next()
                .and_then(PublicOperation::from_segment)
                .ok_or(RouteError::MalformedRoute)?;
            if segments.next().is_some()
                || !valid_identifier(principal)
                || operation.kind() != kind
                || kind == PublicRouteKind::Simulation
            {
                return Err(RouteError::MalformedRoute);
            }
            return Self::for_operation(target, principal, operation);
        }

        let simulation_prefix = format!("{}/clients/", target.simulation_prefix());
        let suffix = key
            .strip_prefix(&simulation_prefix)
            .ok_or(RouteError::WrongRoute)?;
        let mut segments = suffix.split('/');
        let principal = segments.next().ok_or(RouteError::MalformedRoute)?;
        let operation = segments
            .next()
            .and_then(PublicOperation::from_segment)
            .ok_or(RouteError::MalformedRoute)?;
        if segments.next().is_some()
            || !valid_identifier(principal)
            || operation.kind() != PublicRouteKind::Simulation
        {
            return Err(RouteError::MalformedRoute);
        }
        Self::for_operation(target, principal, operation)
    }

    /// The deployment target addressed by this route.
    #[must_use]
    pub fn target(&self) -> &DeploymentTarget {
        &self.target
    }

    /// Return the bounded offer-only bootstrap document for this target.
    #[must_use]
    pub fn session_offers(&self) -> SessionOffers {
        self.target.session_offers()
    }

    /// The principal derived from the protected route.
    #[must_use]
    pub fn principal(&self) -> &str {
        &self.principal
    }

    /// The ACL lane encoded by this route.
    #[must_use]
    pub const fn kind(&self) -> PublicRouteKind {
        self.kind
    }

    /// The exact operation encoded by this route.
    #[must_use]
    pub const fn operation(&self) -> PublicOperation {
        self.operation
    }

    /// The exact concrete key to query or publish through the transport.
    #[must_use]
    pub fn key(&self) -> String {
        if self.kind == PublicRouteKind::Simulation {
            format!(
                "{}/clients/{}/{}",
                self.target.simulation_prefix(),
                self.principal,
                self.operation.segment()
            )
        } else {
            format!(
                "{}/clients/{}/{}/{}",
                self.target.session_prefix(),
                self.principal,
                self.kind.segment(),
                self.operation.segment()
            )
        }
    }
}

impl DeploymentTarget {
    /// Construct an exact principal-bound public session route.
    ///
    /// The returned value owns the validated target and principal so a
    /// transport cannot accidentally combine one target's prefix with another
    /// target's client lane.
    pub fn public_route(
        &self,
        principal: impl Into<String>,
        kind: PublicRouteKind,
    ) -> Result<PublicRoute, RouteError> {
        PublicRoute::new(self, principal, kind)
    }

    /// Construct the one offer served by the v1 bootstrap exchange.
    #[must_use]
    pub fn session_offers(&self) -> SessionOffers {
        SessionOffers {
            sessions: vec![SessionOffer {
                protocol: SESSION_PROTOCOL.to_owned(),
                key_prefix: self.session_prefix(),
            }],
        }
    }

    /// Exact v1 simulation coordination prefix.
    #[must_use]
    pub fn simulation_prefix(&self) -> String {
        format!("{}/simulation/v1", self.prefix())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn target() -> DeploymentTarget {
        DeploymentTarget::new("workshop", "rover-01").expect("target")
    }

    #[test]
    fn route_keys_are_exact_and_principal_is_derived_from_the_key() {
        let target = target();
        assert_eq!(target.session_offers().sessions.len(), 1);
        assert_eq!(
            target.session_offers().sessions[0].key_prefix,
            target.session_prefix()
        );
        let route = target
            .public_route("operator-a", PublicRouteKind::Inspection)
            .and_then(|route| route.with_operation(PublicOperation::Info))
            .expect("route");
        assert_eq!(
            route.key(),
            "phoxal/workshop/supervisors/rover-01/session/v1/clients/operator-a/inspection/info"
        );
        assert_eq!(route.principal(), "operator-a");
        assert_eq!(route.kind(), PublicRouteKind::Inspection);
        assert_eq!(route.operation(), PublicOperation::Info);
        let parsed = PublicRoute::parse(&target, &route.key()).expect("parse");
        assert_eq!(parsed.principal(), "operator-a");
        assert_eq!(parsed.kind(), PublicRouteKind::Inspection);
        assert_eq!(parsed.operation(), PublicOperation::Info);
        for invalid in [
            "phoxal/other/supervisors/rover-01/session/v1/clients/operator-a/inspection",
            "phoxal/workshop/supervisors/rover-01/session/v1/clients/Operator/inspection",
            "phoxal/workshop/supervisors/rover-01/session/v1/clients/operator-a/inspection/extra",
            "phoxal/workshop/supervisors/rover-01/session/v1/clients/operator-a/*",
            "phoxal/workshop/supervisors/rover-01/session/v1/clients/operator-a/inspection/command",
        ] {
            assert!(PublicRoute::parse(&target, invalid).is_err(), "{invalid}");
        }
    }

    #[test]
    fn simulation_routes_use_the_dedicated_v1_prefix() {
        let target = target();
        let route =
            PublicRoute::for_operation(&target, "simulator", PublicOperation::AcquireAuthority)
                .expect("simulation route");
        assert_eq!(
            route.key(),
            "phoxal/workshop/supervisors/rover-01/simulation/v1/clients/simulator/acquire-authority"
        );
        let parsed = PublicRoute::parse(&target, &route.key()).expect("parse simulation route");
        assert_eq!(parsed.kind(), PublicRouteKind::Simulation);
        assert_eq!(parsed.operation(), PublicOperation::AcquireAuthority);
        assert!(PublicRoute::parse(
            &target,
            "phoxal/workshop/supervisors/rover-01/session/v1/clients/simulator/simulation/prepare-boundary"
        )
        .is_err());
    }
}
