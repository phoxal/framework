//! Correlation and completion ownership shared by the input and output adapters.
use crate::runtime::input::{OperationCompletionRecord, ReadError, RequestError, TransportValue};
use crate::runtime::outputs::activation::ActivationKey;
use std::collections::{BTreeMap, BTreeSet};
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub(super) const MAX_EXPIRED_CORRELATIONS: usize = 4096;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum ExchangeKind {
    Read,
    Request,
}

pub(super) struct PendingCorrelation {
    pub(super) field: &'static str,
    pub(super) key: TransportValue,
    pub(super) kind: ExchangeKind,
    pub(super) deadline: Option<Instant>,
    pub(super) reply_admitted: bool,
    pub(super) expected_source: String,
}

pub(super) type CorrelationKey = (String, u64);

pub(super) type CorrelationMap = Arc<Mutex<BTreeMap<CorrelationKey, PendingCorrelation>>>;

pub(super) type ExpiredCorrelationSet = Arc<Mutex<BTreeSet<CorrelationKey>>>;

pub(super) type OperationQueue = Arc<Mutex<Vec<OperationCompletionRecord>>>;

pub(super) enum ExchangeCompletion {
    Read {
        field: &'static str,
        key: TransportValue,
        result: Result<TransportValue, ReadError>,
    },
    Request {
        field: &'static str,
        key: TransportValue,
        result: Result<TransportValue, RequestError>,
    },
}

pub(super) type ExchangeCompletionQueue = Arc<Mutex<Vec<ExchangeCompletion>>>;

pub(super) struct GeneratedCorrelation {
    pub(super) ticket: u64,
    pub(super) expected_source: String,
    pub(super) endpoint: String,
    pub(super) caller: String,
    pub(super) caller_rank: u64,
    pub(super) max_response_bytes: u64,
    pub(super) deadline: Option<Instant>,
}

pub(super) type GeneratedCorrelationMap = Arc<Mutex<BTreeMap<u64, GeneratedCorrelation>>>;

pub(super) type GeneratedCompletionQueue =
    Arc<Mutex<Vec<crate::runtime::input::TransportCallCompletion>>>;

#[derive(Clone)]
pub(super) struct AcceptedActivation {
    pub(super) key: ActivationKey,
    pub(super) attempt: u64,
}

pub(super) type ActivationStateMap = Arc<Mutex<BTreeMap<&'static str, AcceptedActivation>>>;

pub(super) fn not_sent_completion(
    field: &'static str,
    key: TransportValue,
    kind: ExchangeKind,
    reason: impl Into<String>,
) -> ExchangeCompletion {
    let reason = reason.into();
    match kind {
        ExchangeKind::Read => ExchangeCompletion::Read {
            field,
            key,
            result: Err(ReadError::NotSent(reason)),
        },
        ExchangeKind::Request => ExchangeCompletion::Request {
            field,
            key,
            result: Err(RequestError::NotSent(reason)),
        },
    }
}
