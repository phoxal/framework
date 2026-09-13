use phoxal_motion::ApplyEmergencyResponse;

/// Fresh per-invocation Motion products.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct MotionOutputs {
    /// One processing reply for every admitted emergency or arm command.
    #[phoxal::runtime::outputs::reply(emergency, max_items = 32, max_bytes = 16_384)]
    pub emergency_replies: Vec<phoxal::runtime::Reply<ApplyEmergencyResponse>>,
}
