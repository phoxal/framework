use crate::api::navigation::v1::{
    ApplyCommandResponse, GetGoalStatusResponse, GoalFinished, navigation,
};

/// Fresh per-invocation Navigation products.
#[phoxal::runtime::outputs]
#[derive(Default)]
pub struct NavigationOutputs {
    /// One processing reply for every admitted command.
    #[phoxal::runtime::outputs::reply(commands, max_items = 32, max_bytes = 16_384)]
    pub replies: Vec<phoxal::runtime::Reply<ApplyCommandResponse>>,
    /// One correlated result for every admitted goal-status call.
    #[phoxal::runtime::outputs::reply(status_calls, max_items = 32, max_bytes = 16_384)]
    pub status_replies: Vec<phoxal::runtime::Reply<GetGoalStatusResponse>>,
    /// Terminal goal transitions produced by this invocation.
    #[phoxal::runtime::outputs::event(
        port = navigation::methods::FINISHED.__event_port(),
        max_items = 64,
        max_bytes = 16_384
    )]
    pub finished: Vec<GoalFinished>,
}
