//! Conveniences over the generated contract bodies.
//!
//! Everything declared here is an inherent `impl` on a body that
//! `phoxal_api_tree!` already generated: a constructor for a canonical value, a
//! predicate over a body's own fields, or a const a consumer of that contract
//! must apply. Each item lives beside its contract because more than one
//! participant needs the same answer, and two participants deriving it
//! independently is coupling with nothing holding it together - the moment they
//! disagree, they disagree about the meaning of the wire.
//!
//! What must **not** land here is participant policy. An item in this module may
//! read only the body it is implemented on; anything that depends on a
//! participant's configuration, its clock, its position in the graph or its
//! arbitration rules belongs to that participant, not to the contract. The
//! module is deliberately private: these are inherent impls, so each item is
//! still reached at exactly one canonical path on its own type
//! (`v0_2::drive::Target::stopped`), and there is no second way to name it.
//!
//! The wire is untouched by construction. Inherent impls add no field, no
//! variant and no serde attribute, so nothing here can change how a body
//! encodes.

use std::time::Duration;

use crate::{v0_1, v0_2};

impl v0_1::drive::Target {
    /// The target that commands no motion.
    ///
    /// This is the value every producer in the drive chain publishes when it has
    /// nothing to command - on arbitration finding no candidate, and on the
    /// drive service withdrawing actuation authority - so it is written once
    /// here instead of being spelled out per participant.
    #[must_use]
    pub const fn stopped() -> Self {
        Self {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
            curvature_limit_radpm: None,
        }
    }
}

impl v0_2::drive::Target {
    /// Construct a target only when both planar velocity components are finite.
    pub fn try_new(linear_x_mps: f32, angular_z_radps: f32) -> Result<Self, InvalidTarget> {
        if !linear_x_mps.is_finite() {
            return Err(InvalidTarget::LinearXNotFinite);
        }
        if !angular_z_radps.is_finite() {
            return Err(InvalidTarget::AngularZNotFinite);
        }
        Ok(Self {
            linear_x_mps,
            angular_z_radps,
        })
    }

    /// The finite forward velocity component in metres per second.
    #[must_use]
    pub const fn linear_x_mps(&self) -> f32 {
        self.linear_x_mps
    }

    /// The finite yaw velocity component in radians per second.
    #[must_use]
    pub const fn angular_z_radps(&self) -> f32 {
        self.angular_z_radps
    }

    /// The target that commands no motion.
    #[must_use]
    pub const fn stopped() -> Self {
        Self {
            linear_x_mps: 0.0,
            angular_z_radps: 0.0,
        }
    }
}

/// Why a control target could not be constructed.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InvalidTarget {
    LinearXNotFinite,
    AngularZNotFinite,
}

impl std::fmt::Display for InvalidTarget {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let field = match self {
            Self::LinearXNotFinite => "linear_x_mps",
            Self::AngularZNotFinite => "angular_z_radps",
        };
        write!(formatter, "control target field {field} must be finite")
    }
}

impl std::error::Error for InvalidTarget {}

impl v0_1::localize::LocalizationState {
    /// Whether this estimate is a pose a consumer may act on.
    ///
    /// A publication of this contract is not by itself a fix. The body has no
    /// separate validity flag, so usability is read off the numbers:
    ///
    /// - `x_m`, `y_m` and `yaw_rad` must be finite. A NaN or infinite pose does
    ///   not fail loudly downstream; it propagates silently through every
    ///   geometric computation that consumes it and turns a planned path or a
    ///   steering correction into NaN.
    /// - `confidence` must be finite for the same reason, and because a NaN
    ///   confidence compares `false` against every threshold. Without this
    ///   clause a NaN would be rejected by accident rather than on purpose,
    ///   which is not a property to depend on.
    /// - `confidence` must be strictly positive. This is the clause that
    ///   separates "publishing, but not localized" from a real fix: a localizer
    ///   that is alive and has converged on nothing publishes a well-formed
    ///   state at zero confidence, and a consumer that ignored this would steer
    ///   on the origin.
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.x_m.is_finite()
            && self.y_m.is_finite()
            && self.yaw_rad.is_finite()
            && self.confidence.is_finite()
            && self.confidence > 0.0
    }
}

impl v0_1::navigation::RequestId {
    /// Whether this identifier is well formed enough to be echoed back on the
    /// wire and used as a map key.
    ///
    /// The rule is deliberately narrow because the value is chosen by the
    /// caller and is then reflected in published progress and result bodies: a
    /// non-empty, bounded, ASCII alphanumeric-plus-`-_.` token. That excludes
    /// control characters and unbounded input from a contract every consumer of
    /// the navigation tree reads, and it keeps the identifier safe to render in
    /// logs and keys without escaping.
    ///
    /// Surrounding whitespace is ignored when judging emptiness, so a value of
    /// only spaces is refused rather than accepted as a distinct request.
    ///
    /// This says nothing about *authority* - whether the caller is entitled to
    /// act on the request it names is a separate question the contract does not
    /// answer.
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let value = self.value.trim();
        !value.is_empty()
            && value.len() <= 128
            && value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    }
}

impl v0_1::component::encoder::Sample {
    /// How long a consumer may keep using the newest sample before it must treat
    /// the encoder as silent.
    ///
    /// The horizon belongs to the contract rather than to each consumer because
    /// every consumer of one encoder has to agree on it. Two consumers choosing
    /// their own would disagree about whether the same wheel is turning, and the
    /// disagreement would be invisible - both would look internally consistent.
    ///
    /// A consumer that has seen nothing newer than this must drop the encoder
    /// from its estimate rather than keep integrating the last velocity, which
    /// would let a dead publisher move the robot's idea of itself forever.
    /// Drivers publish well above this rate, so the horizon only trips on a
    /// genuinely silent publisher and not on ordinary jitter.
    pub const STALE_AFTER: Duration = Duration::from_millis(200);
}

impl v0_2::localize::LocalizationState {
    #[must_use]
    pub fn is_usable(&self) -> bool {
        self.x_m.is_finite()
            && self.y_m.is_finite()
            && self.yaw_rad.is_finite()
            && self.confidence.is_finite()
            && self.confidence > 0.0
    }
}

impl v0_2::navigation::RequestId {
    #[must_use]
    pub fn is_valid(&self) -> bool {
        let value = self.value.trim();
        !value.is_empty()
            && value.len() <= 128
            && value
                .chars()
                .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    }
}

impl v0_2::component::encoder::Sample {
    pub const STALE_AFTER: Duration = Duration::from_millis(200);
}

