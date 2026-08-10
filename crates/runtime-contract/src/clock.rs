//! The persisted execution time domain shared by bundles and process protocols.

use crate::wire_schema::{DescribeWire, EnumRepresentation, VariantBody, WireSchema, WireVariant};

/// The time domain one compiled robot executes in.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, serde::Deserialize, serde::Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Clock {
    /// Host boot-anchored time driven by real hardware.
    #[default]
    Real,
    /// Time published by a simulation world authority.
    Simulated,
}

impl Clock {
    /// The wire token for this domain, identical to the `snake_case` rename
    /// serde derives.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Real => "real",
            Self::Simulated => "simulated",
        }
    }

    /// Every domain, so the wire declaration and `as_str` cannot cover
    /// different sets.
    const ALL: [Self; 2] = [Self::Real, Self::Simulated];
}

// Hand-written for the same reason as the rest of this crate's declarations:
// the process-contract floor sits below `phoxal-macros`, so it cannot use the
// derive that reads these serde attributes.
impl DescribeWire for Clock {
    // Invariant: this states what the derived `Serialize` above writes - one
    // externally tagged unit variant per domain, spelled by the `snake_case`
    // rename that `as_str` also returns.
    fn wire_schema() -> WireSchema {
        WireSchema::enumeration(
            EnumRepresentation::ExternallyTagged,
            Clock::ALL.map(|clock| WireVariant::new(clock.as_str(), VariantBody::Unit)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The declared shape is checked against a real serialized value, so the
    /// hand-written declaration cannot drift from the derive beside it.
    #[test]
    fn the_declared_shape_is_the_shape_serde_writes() {
        for clock in Clock::ALL {
            let json = serde_json::to_value(clock).expect("a clock domain serializes");
            assert_eq!(json, serde_json::Value::from(clock.as_str()));
            assert_eq!(Clock::wire_schema().conforms(&json), Ok(()));
        }
    }
}
