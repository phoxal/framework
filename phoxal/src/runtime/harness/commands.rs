//! Bounded ingress and reply correlation owned by the canonical harness.

use super::{HarnessCall, HarnessError};
use crate::contracts::ProstPayload;
use crate::runtime::{Command, CommandId, CommandOrder, Commands, Reply};

/// Storage used by generated operation bindings in a harness endpoint view.
/// Received wire order stays intact; a separate token identifies each harness call.
pub struct HarnessCommands<Request, Response> {
    pending: Vec<Command<Request, Response>>,
    pending_bytes: usize,
    correlations: Vec<(CommandOrder, CommandId)>,
    replies: Vec<(CommandId, Vec<u8>)>,
    reply_bytes: usize,
}

impl<Request, Response> Default for HarnessCommands<Request, Response> {
    fn default() -> Self {
        Self {
            pending: Vec::new(),
            pending_bytes: 0,
            correlations: Vec::new(),
            replies: Vec::new(),
            reply_bytes: 0,
        }
    }
}

impl<Request: ProstPayload, Response: ProstPayload> HarnessCommands<Request, Response> {
    /// Reserves pending capacity before issuing a unique harness correlation.
    #[allow(clippy::too_many_arguments)]
    pub fn push(
        &mut self,
        command: Command<Request, Response>,
        next_call: &mut u64,
        owner: u64,
        endpoint: &'static str,
        max_items: u64,
        max_bytes: u64,
    ) -> Result<HarnessCall<Response>, HarnessError> {
        let bytes = command
            .request()
            .encode_payload()
            .map_err(|_| HarnessError::PendingUnencodable { endpoint })?
            .len();
        if self.pending.len() as u64 >= max_items
            || self.correlations.len() as u64 >= max_items
            || self.pending_bytes.saturating_add(bytes) as u64 > max_bytes
        {
            return Err(HarnessError::PendingFull { endpoint });
        }
        if self
            .correlations
            .iter()
            .any(|(order, _)| *order == command.order())
        {
            return Err(HarnessError::DuplicateCommand { endpoint });
        }
        let successor = next_call
            .checked_add(1)
            .ok_or(HarnessError::CorrelationExhausted)?;
        let id = CommandId::new(*next_call);
        self.correlations.push((command.order(), id));
        self.pending.push(command);
        self.pending_bytes += bytes;
        *next_call = successor;
        Ok(HarnessCall::new(id, owner))
    }

    /// Freezes the received batch once at the next declared release.
    pub fn freeze(&mut self) -> Commands<Request, Response> {
        self.pending_bytes = 0;
        Commands::new(std::mem::take(&mut self.pending))
    }

    /// Reserves the whole candidate's replies before owner acceptance.
    pub fn validate(
        &self,
        replies: &[Reply<Response>],
        max_items: u64,
        max_bytes: u64,
    ) -> crate::Result<()> {
        let mut orders = std::collections::BTreeSet::new();
        let mut bytes = self.reply_bytes;
        for reply in replies {
            if !orders.insert(reply.order())
                || !self
                    .correlations
                    .iter()
                    .any(|(order, _)| *order == reply.order())
            {
                anyhow::bail!("candidate reply has no unique harness correlation");
            }
            bytes = bytes
                .checked_add(reply.response().encode_payload()?.len())
                .ok_or(HarnessError::RetainedFull)?;
        }
        if self.replies.len().saturating_add(replies.len()) as u64 > max_items
            || bytes as u64 > max_bytes
        {
            return Err(HarnessError::RetainedFull.into());
        }
        Ok(())
    }

    /// Retains replies from a candidate already reserved and accepted by the owner.
    pub fn capture(&mut self, replies: &[Reply<Response>]) -> crate::Result<()> {
        for reply in replies {
            let bytes = reply.response().encode_payload()?;
            let position = self
                .correlations
                .iter()
                .position(|(order, _)| *order == reply.order())
                .ok_or_else(|| anyhow::anyhow!("accepted reply has no harness correlation"))?;
            let (_, id) = self.correlations.remove(position);
            self.reply_bytes += bytes.len();
            self.replies.push((id, bytes));
        }
        Ok(())
    }

    /// Takes one accepted reply and releases its retained byte budget.
    pub fn take_reply(&mut self, id: CommandId) -> Option<Vec<u8>> {
        let position = self
            .replies
            .iter()
            .position(|(reply_id, _)| *reply_id == id)?;
        let (_, bytes) = self.replies.remove(position);
        self.reply_bytes -= bytes.len();
        Some(bytes)
    }

    /// Reports whether a correlation still belongs to a future input cut.
    pub fn is_staged(&self, id: CommandId) -> bool {
        self.correlations.iter().any(|(order, correlation)| {
            *correlation == id && self.pending.iter().any(|command| command.order() == *order)
        })
    }

    /// Discards pending calls and accepted effects on owner reset.
    pub fn clear(&mut self) {
        self.pending.clear();
        self.pending_bytes = 0;
        self.correlations.clear();
        self.replies.clear();
        self.reply_bytes = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::contracts::Empty;

    fn command(order: CommandOrder) -> Command<Empty, Empty> {
        Command::with_source_order(order, "operator", Empty {})
    }

    #[test]
    fn duplicate_orders_refuse_before_acceptance_and_reset_cannot_alias_tokens() {
        let mut queue = HarnessCommands::<Empty, Empty>::default();
        let order = CommandOrder::new(7, 3, CommandId::new(99));
        let mut counter = 0;
        let first = queue
            .push(command(order), &mut counter, 1, "start", 4, 64)
            .unwrap();
        assert!(matches!(
            queue.push(command(order), &mut counter, 1, "start", 4, 64),
            Err(HarnessError::DuplicateCommand { .. })
        ));
        assert_eq!(counter, 1);
        let cut = queue.freeze();
        assert_eq!(cut.items()[0].order(), order);
        assert_eq!(cut.items()[0].source(), "operator");
        let reply = || Reply::with_order(order, Empty {});
        assert!(queue.validate(&[reply(), reply()], 4, 64).is_err());
        assert!(queue.take_reply(first.id).is_none());
        queue.clear();
        let fresh = queue
            .push(command(order), &mut counter, 1, "start", 4, 64)
            .unwrap();
        assert_ne!(first.id, fresh.id);
        queue.freeze();
        queue.validate(&[reply()], 4, 64).unwrap();
        queue.capture(&[reply()]).unwrap();
        assert!(queue.take_reply(first.id).is_none());
        assert!(queue.take_reply(fresh.id).is_some());
        assert!(queue.take_reply(fresh.id).is_none());
    }

    #[test]
    fn correlation_exhaustion_mutates_neither_pending_inputs_nor_counter() {
        let mut queue = HarnessCommands::<Empty, Empty>::default();
        let mut counter = u64::MAX;
        let order = CommandOrder::new(0, 0, CommandId::new(1));
        assert!(matches!(
            queue.push(command(order), &mut counter, 1, "start", 4, 64),
            Err(HarnessError::CorrelationExhausted)
        ));
        assert_eq!(counter, u64::MAX);
        assert!(queue.freeze().items().is_empty());
        assert!(queue.correlations.is_empty());
    }
}
