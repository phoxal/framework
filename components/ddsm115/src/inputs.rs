use phoxal::runtime::input::Setpoint;

/// DDSM115 inputs and observations admitted at one Runtime boundary.
#[phoxal::runtime::inputs]
pub struct Ddsm115Inputs {
    /// The final motion authority's expiring intent for this actuator.
    pub actuator: Setpoint<phoxal_service_motion::ActuatorSetpoint>,
}
