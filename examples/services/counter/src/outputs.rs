use crate::runtime::Counter;
use example_counter_service::{CounterState, counter};

#[phoxal::runtime::outputs]
pub struct CounterOutputs {}

#[phoxal::runtime::outputs]
impl Counter {
    /// Publishes the current value through the owner-generated State port.
    #[phoxal::runtime::outputs::state(
        port = counter::STATE,
        max_bytes = 64,
        bootstrap,
        on_change,
    )]
    fn state(&self, state: &u64) -> CounterState {
        CounterState { value: *state }
    }
}
