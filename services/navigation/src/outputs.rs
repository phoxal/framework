use phoxal_service_navigation::{ApplyCommandResponse, GoalFinished, ports};

/// Fresh per-invocation Navigation products.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct NavigationOutputs {
    /// One processing reply for every admitted command.
    #[phoxal::runtime::outputs::reply(commands, max_items = 32, max_bytes = 16_384)]
    pub replies: Vec<phoxal::runtime::Reply<ApplyCommandResponse>>,
    /// Terminal goal transitions produced by this invocation.
    #[phoxal::runtime::outputs::event(
        port = ports::FINISHED,
        max_items = 64,
        max_bytes = 16_384
    )]
    pub finished: Vec<GoalFinished>,
}
