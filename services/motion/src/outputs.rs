use crate::api::motion::v1::ApplyEmergencyResponse;

/// Fresh per-invocation Motion products.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct MotionOutputs {
    #[phoxal::runtime::outputs::reply(arm, max_items = 32, max_bytes = 16_384)]
    pub arm_replies: Vec<phoxal::runtime::Reply<ApplyEmergencyResponse>>,
    #[phoxal::runtime::outputs::reply(disarm, max_items = 32, max_bytes = 16_384)]
    pub disarm_replies: Vec<phoxal::runtime::Reply<ApplyEmergencyResponse>>,
    #[phoxal::runtime::outputs::reply(engage_emergency, max_items = 32, max_bytes = 16_384)]
    pub engage_emergency_replies: Vec<phoxal::runtime::Reply<ApplyEmergencyResponse>>,
    #[phoxal::runtime::outputs::reply(release_emergency, max_items = 32, max_bytes = 16_384)]
    pub release_emergency_replies: Vec<phoxal::runtime::Reply<ApplyEmergencyResponse>>,
}
