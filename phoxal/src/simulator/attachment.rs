//! Source-bound simulation attachment transaction.

use super::*;

/// A source-bound host transaction from observed Preparing to Active commit.
pub struct SimulationAttachTransaction {
    pub(super) initial: SimulationAttachmentState,
    pub(super) request: AttachRequest,
    pub(super) host: ProducerId,
    pub(super) time_domain: TimeDomain,
    pub(super) response: Option<AttachTransactionResponse>,
    pub(super) transaction_liveliness: Option<KeyLivelinessToken>,
    pub(super) attachment_liveliness: Arc<tokio::sync::Mutex<Option<KeyLivelinessToken>>>,
    pub(super) end: Querier<EndRequest>,
}

pub(super) enum AttachTransactionResponse {
    Pending(tokio::task::JoinHandle<Result<AttachResponse, QueryError>>),
    Complete(AttachResponse),
}

impl SimulationAttachTransaction {
    /// The ordered attachment state observed before this handle was returned.
    /// A new transaction is Preparing. An idempotent retry may already be
    /// Active and completes immediately.
    #[must_use]
    pub const fn initial(&self) -> SimulationAttachmentState {
        self.initial
    }

    /// Await and validate the supervisor's Active commit.
    pub async fn commit(mut self) -> Result<AttachResponse, SimulatorError> {
        let response = match self
            .response
            .take()
            .ok_or_else(|| SimulatorError::AttachmentTask {
                detail: "the attachment transaction response was already consumed".to_owned(),
            })? {
            AttachTransactionResponse::Pending(task) => {
                task.await
                    .map_err(|error| SimulatorError::AttachmentTask {
                        detail: error.to_string(),
                    })??
            }
            AttachTransactionResponse::Complete(response) => response,
        };
        validate_attach_response(response, self.request, self.host, self.time_domain)?;
        let lease =
            self.transaction_liveliness
                .take()
                .ok_or_else(|| SimulatorError::AttachmentTask {
                    detail: "the attachment transaction lease was already consumed".to_owned(),
                })?;
        *self.attachment_liveliness.lock().await = Some(lease);
        Ok(response)
    }

    /// Abort this Preparing transaction, revoke its no-late-commit lease, and
    /// await the supervisor's source-bound Removing response.
    pub async fn abort(
        mut self,
        reason: SimulationEndReason,
    ) -> Result<EndResponse, SimulatorError> {
        if let Some(AttachTransactionResponse::Pending(task)) = self.response.take() {
            task.abort();
        }
        drop(self.transaction_liveliness.take());
        let response = self.end.query(EndRequest { reason }).await?;
        if response.attachment.phase != SimulationAttachmentPhase::Removing
            || response.attachment.host != self.host
        {
            return Err(SimulatorError::AttachmentProtocol {
                detail: "attachment abort did not converge to Removing under this host producer"
                    .to_owned(),
            });
        }
        Ok(response)
    }
}

impl Drop for SimulationAttachTransaction {
    fn drop(&mut self) {
        if let Some(AttachTransactionResponse::Pending(task)) = &self.response {
            task.abort();
        }
        // Dropping this token is the synchronous cancellation fence. The
        // supervisor observes it while Preparing and cannot activate after it
        // disappears, even though Zenoh may still deliver the abandoned query.
        self.transaction_liveliness.take();
    }
}
pub(super) fn validate_attach_response(
    response: AttachResponse,
    request: AttachRequest,
    host: ProducerId,
    time_domain: TimeDomain,
) -> Result<(), SimulatorError> {
    let attachment = response.attachment;
    if response.time_domain != time_domain
        || attachment.phase != SimulationAttachmentPhase::Active
        || attachment.host != host
        || attachment.controller != request.controller()
        || attachment.world != request.world()
        || attachment.attached_at.world != request.progress()
    {
        return Err(SimulatorError::AttachmentProtocol {
            detail: "attach response did not preserve the requested source binding, progress boundary, and monotonic domain".to_owned(),
        });
    }
    Ok(())
}
