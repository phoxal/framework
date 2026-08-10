//! The release a contract change requires, and whether a version clears it.

use std::fmt;

use semver::Version;

use crate::surface::CompatibilityImpact;

/// The smallest release that may carry a change of a given impact.
#[derive(Clone, Copy, Debug, Eq, Ord, PartialEq, PartialOrd)]
pub(crate) enum ReleaseStep {
    /// The next patch.
    Patch,
    /// The next minor.
    Minor,
    /// The next major.
    Major,
}

impl ReleaseStep {
    /// The step an impact requires over `baseline`.
    ///
    /// Before 1.0 the minor *is* the compatibility line: SemVer gives a 0.x
    /// release no major axis to break on, so an addition and a break both land
    /// on the next minor and are told apart by the changelog rather than by the
    /// number. From 1.0 on a break is a major and nothing else.
    pub(crate) fn required(impact: CompatibilityImpact, baseline: &Version) -> Self {
        match impact {
            CompatibilityImpact::Unchanged => Self::Patch,
            CompatibilityImpact::Additive => Self::Minor,
            CompatibilityImpact::Breaking if baseline.major == 0 => Self::Minor,
            CompatibilityImpact::Breaking => Self::Major,
        }
    }

    /// The lowest version that is this step over `baseline`.
    pub(crate) fn minimum_version(self, baseline: &Version) -> Version {
        match self {
            Self::Patch => Version::new(baseline.major, baseline.minor, baseline.patch + 1),
            Self::Minor => Version::new(baseline.major, baseline.minor + 1, 0),
            Self::Major => Version::new(baseline.major + 1, 0, 0),
        }
    }

    /// Whether `candidate` is at least this step over `baseline`.
    ///
    /// Anything above the minimum passes. Releasing further than a change
    /// strictly requires is a maintainer's call - the checker only refuses a
    /// release that under-states what changed.
    pub(crate) fn is_satisfied_by(self, baseline: &Version, candidate: &Version) -> bool {
        *candidate >= self.minimum_version(baseline)
    }

    /// This step's spelling in a report.
    fn token(self) -> &'static str {
        match self {
            Self::Patch => "patch",
            Self::Minor => "minor",
            Self::Major => "major",
        }
    }
}

impl fmt::Display for ReleaseStep {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.token())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pre_v1() -> Version {
        Version::new(0, 58, 1)
    }

    fn post_v1() -> Version {
        Version::new(1, 4, 2)
    }

    /// An unchanged surface constrains nothing beyond releasing at all.
    #[test]
    fn an_unchanged_surface_allows_a_patch() {
        let step = ReleaseStep::required(CompatibilityImpact::Unchanged, &pre_v1());
        assert_eq!(step, ReleaseStep::Patch);
        assert_eq!(step.minimum_version(&pre_v1()), Version::new(0, 58, 2));
        assert!(step.is_satisfied_by(&pre_v1(), &Version::new(0, 58, 2)));
        assert!(!step.is_satisfied_by(&pre_v1(), &pre_v1()));
    }

    /// Before 1.0 both an addition and a break move the minor, because that is
    /// the only compatibility line SemVer gives a 0.x release.
    #[test]
    fn before_one_point_zero_every_contract_change_is_a_minor() {
        assert_eq!(
            ReleaseStep::required(CompatibilityImpact::Additive, &pre_v1()),
            ReleaseStep::Minor
        );
        assert_eq!(
            ReleaseStep::required(CompatibilityImpact::Breaking, &pre_v1()),
            ReleaseStep::Minor
        );
    }

    /// From 1.0 on the two separate: an addition is a minor and a break is a
    /// major.
    #[test]
    fn after_one_point_zero_a_break_is_a_major() {
        assert_eq!(
            ReleaseStep::required(CompatibilityImpact::Additive, &post_v1()),
            ReleaseStep::Minor
        );
        let step = ReleaseStep::required(CompatibilityImpact::Breaking, &post_v1());
        assert_eq!(step, ReleaseStep::Major);
        assert_eq!(step.minimum_version(&post_v1()), Version::new(2, 0, 0));
        assert!(!step.is_satisfied_by(&post_v1(), &Version::new(1, 5, 0)));
        assert!(step.is_satisfied_by(&post_v1(), &Version::new(2, 0, 0)));
    }

    /// A patch cannot carry an addition, the next minor can, and releasing
    /// further than required stays the maintainer's call.
    #[test]
    fn an_additive_release_needs_the_next_minor_and_accepts_more() {
        let step = ReleaseStep::required(CompatibilityImpact::Additive, &pre_v1());
        assert!(!step.is_satisfied_by(&pre_v1(), &Version::new(0, 58, 2)));
        assert!(step.is_satisfied_by(&pre_v1(), &Version::new(0, 59, 0)));
        assert!(step.is_satisfied_by(&pre_v1(), &Version::new(0, 60, 0)));
        assert!(step.is_satisfied_by(&pre_v1(), &Version::new(1, 0, 0)));
    }
}
