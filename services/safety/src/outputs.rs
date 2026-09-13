/// Safety emits only state projections.  Motion consumes the constraints
/// state and remains the sole service that emits final actuator intent.
pub type SafetyOutputs = ();
