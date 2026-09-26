//! Generated, owned immutable query projections.

use crate::runtime::{
    StepContext,
    transport::{PreparedOutput, WireSample},
};

/// One generated Read handler with its owned accepted projection.
///
/// The closure may capture the immutable service and its owned view, but cannot
/// borrow the runtime's State. The runner owns admission and worker lifetime.
pub struct ReadView {
    pub(crate) field: &'static str,
    handler: Box<ReadHandler>,
}

type ReadHandler =
    dyn Fn(&WireSample, StepContext, &str) -> crate::Result<PreparedOutput> + Send + Sync;

impl ReadView {
    /// Binds a generated handler to an owned, thread-safe projection.
    pub fn new(
        field: &'static str,
        handler: impl Fn(&WireSample, StepContext, &str) -> crate::Result<PreparedOutput>
        + Send
        + Sync
        + 'static,
    ) -> Self {
        Self {
            field,
            handler: Box::new(handler),
        }
    }

    pub(crate) fn respond(
        &self,
        sample: &WireSample,
        context: StepContext,
        source: &str,
    ) -> crate::Result<PreparedOutput> {
        (self.handler)(sample, context, source)
    }
}
