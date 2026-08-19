crate::endpoints! {
    state: State<State>;
}

#[derive(
    phoxal_macros::DescribeWire, Clone, Debug, PartialEq, serde::Serialize, serde::Deserialize,
)]
#[serde(try_from = "StateWire")]
pub struct State {
    pub voltage_v: f32,
    pub current_a: f32,
    pub charge_ratio: f32,
}
#[derive(serde::Deserialize)]
struct StateWire {
    voltage_v: f32,
    current_a: f32,
    charge_ratio: f32,
}
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InvalidState(&'static str);
impl std::fmt::Display for InvalidState {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}
impl std::error::Error for InvalidState {}
impl State {
    pub fn try_new(
        voltage_v: f32,
        current_a: f32,
        charge_ratio: f32,
    ) -> Result<Self, InvalidState> {
        if !(voltage_v.is_finite()
            && voltage_v >= 0.0
            && current_a.is_finite()
            && charge_ratio.is_finite()
            && (0.0..=1.0).contains(&charge_ratio))
        {
            return Err(InvalidState(
                "battery voltage must be nonnegative and all values finite with charge in [0, 1]",
            ));
        }
        Ok(Self {
            voltage_v,
            current_a,
            charge_ratio,
        })
    }
}
impl TryFrom<StateWire> for State {
    type Error = InvalidState;
    fn try_from(v: StateWire) -> Result<Self, Self::Error> {
        Self::try_new(v.voltage_v, v.current_a, v.charge_ratio)
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn constructor_bounds_charge_and_voltage() {
        assert!(State::try_new(-1.0, 0.0, 0.5).is_err());
        assert!(State::try_new(1.0, 0.0, 1.01).is_err());
    }
}
