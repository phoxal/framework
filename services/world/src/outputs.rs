use crate::api::world::v1::WindowResponse;

/// Fresh correlated World call results.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct WorldOutputs {
    /// One result for every admitted revision-aware window call.
    #[phoxal::runtime::outputs::reply(windows, max_items = 32, max_bytes = 262_144)]
    pub window_replies: Vec<phoxal::runtime::Reply<WindowResponse>>,
}
