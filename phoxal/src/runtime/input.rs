//! Typed input snapshots and managed completion accessors.
//!
//! The generic input forms describe what a runtime receives at an invocation
//! boundary.  They are ordinary owned values, so a direct test adapter and a
//! transport runner can construct the same immutable cut without exposing a
//! receiver, socket, or background task to service code.

use std::fmt;
use std::marker::PhantomData;

use super::{ExecutionTime, ObservationStamp, Sample};

/// The input kind fixed by one runtime input form.
#[derive(Clone, Copy, Debug, Eq, Hash, PartialEq)]
pub enum InputKind {
    /// A latest snapshot.
    Latest,
    /// Ordered captured observations.
    Samples,
    /// Ordered occurrences.
    Events,
    /// A replaceable intent.
    Setpoint,
    /// Ordered stream records.
    Stream,
    /// Ordered behavioral commands.
    Commands,
    /// A keyed immutable read completion.
    Read,
    /// A keyed behavioral request completion.
    Request,
    /// A keyed local operation completion.
    Operation,
}

/// Compile-time metadata emitted by `#[phoxal::runtime::inputs]`.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct InputField {
    /// The private Rust field name used by service code.
    pub name: &'static str,
    /// The input semantic kind.
    pub kind: InputKind,
    /// Optional latest-value age bound.
    pub max_age_ms: Option<u64>,
    /// Optional item-count bound for an input batch.
    pub max_items: Option<u64>,
    /// Optional encoded-byte bound for an input batch or response.
    pub max_bytes: Option<u64>,
    /// Optional served Commands descriptor name.
    pub port: Option<&'static str>,
}

/// A type-level marker implemented by every supported input form.
pub trait InputSpec {
    /// The semantic kind supplied by this input type.
    const KIND: InputKind;
}

/// Metadata for one `#[phoxal::runtime::inputs]` declaration.
pub trait InputSet: 'static {
    /// The statically declared fields in source order.
    const FIELDS: &'static [InputField];
}

/// Constructs the first empty input cut for a runtime process.
///
/// The empty cut is an explicit absence for every input form.  A transport
/// owner replaces it with an admitted cut before invoking a service; it is
/// useful to keep the construction rule on the typed input set so a runner
/// never has to deserialize or invent service-owned payload values.
pub trait InputSnapshot: InputSet {
    /// Build an immutable cut with no admitted observations or operations.
    fn empty() -> Self;
}

/// A fixed bound for one frozen or pending batch.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Capacity {
    max_items: u64,
    max_bytes: u64,
}

impl Capacity {
    /// Creates a positive item and encoded-byte bound.
    pub const fn new(max_items: u64, max_bytes: u64) -> Result<Self, CapacityError> {
        if max_items == 0 {
            return Err(CapacityError::ZeroItems);
        }
        if max_bytes == 0 {
            return Err(CapacityError::ZeroBytes);
        }
        Ok(Self {
            max_items,
            max_bytes,
        })
    }

    /// Returns the maximum item count.
    #[must_use]
    pub const fn max_items(self) -> u64 {
        self.max_items
    }

    /// Returns the maximum encoded body size.
    #[must_use]
    pub const fn max_bytes(self) -> u64 {
        self.max_bytes
    }

    /// Checks one complete batch before invocation acceptance.
    pub const fn check(self, items: u64, bytes: u64) -> Result<(), CapacityError> {
        if items > self.max_items {
            return Err(CapacityError::ItemsExceeded {
                limit: self.max_items,
                actual: items,
            });
        }
        if bytes > self.max_bytes {
            return Err(CapacityError::BytesExceeded {
                limit: self.max_bytes,
                actual: bytes,
            });
        }
        Ok(())
    }
}

/// A typed failure to reserve a declared batch capacity.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CapacityError {
    /// A declared item bound was zero.
    #[error("capacity max_items must be positive")]
    ZeroItems,
    /// A declared byte bound was zero.
    #[error("capacity max_bytes must be positive")]
    ZeroBytes,
    /// The complete batch exceeded its item bound.
    #[error("batch contains {actual} items but capacity is {limit}")]
    ItemsExceeded {
        /// Declared maximum.
        limit: u64,
        /// Observed batch size.
        actual: u64,
    },
    /// The complete batch exceeded its encoded-byte bound.
    #[error("batch contains {actual} bytes but capacity is {limit}")]
    BytesExceeded {
        /// Declared maximum.
        limit: u64,
        /// Observed encoded size.
        actual: u64,
    },
}

/// Associates a generated input field with its complete Rust input form.
///
/// The numeric parameter is generated from the authored field identifier by
/// the `inputs` collector.  It keeps output activation checks type-directed
/// even when the input and output declarations live in different modules.
#[doc(hidden)]
pub trait InputFieldBinding<const ID: u64>: InputSet {
    /// The concrete input form carried by this field.
    type Form: InputSpec;
}

/// Marks an input form that can be selected by an output activation method.
#[doc(hidden)]
pub trait ActivationInput {
    /// The activation key type.
    type Key: 'static;
}

impl<Key: 'static, Request: 'static, Response: 'static> ActivationInput
    for Read<Key, Request, Response>
{
    type Key = Key;
}

impl<Key: 'static, RequestBody: 'static, Response: 'static> ActivationInput
    for Request<Key, RequestBody, Response>
{
    type Key = Key;
}

impl<Key: 'static, Response: 'static> ActivationInput for Operation<Key, Response> {
    type Key = Key;
}

/// Connects an activation method's return type to its selected input form.
#[doc(hidden)]
pub trait ActivationFor<Input>: 'static {}

impl<Key: 'static, Request: 'static, Response: 'static> ActivationFor<Read<Key, Request, Response>>
    for Option<Activation<Key, Request>>
{
}

impl<Key: 'static, RequestBody: 'static, Response: 'static>
    ActivationFor<Request<Key, RequestBody, Response>> for Option<Activation<Key, RequestBody>>
{
}

impl<Key: 'static, Response: 'static, WorkerInput: 'static> ActivationFor<Operation<Key, Response>>
    for Option<Activation<Key, WorkerInput>>
{
}

/// Connects a synchronous operation worker's return type to its input form.
#[doc(hidden)]
pub trait OperationWorkerFor<Input>: 'static {}

impl<Key: 'static, Response: 'static, Error: 'static> OperationWorkerFor<Operation<Key, Response>>
    for Result<Response, Error>
{
}

/// The policy requirements implied by one activation input form.
#[doc(hidden)]
pub trait ActivationPolicy {
    /// Whether an activation must author a host-monotonic timeout.
    const REQUIRES_TIMEOUT: bool;
    /// Whether same-key refresh is meaningful for the form.
    const ALLOWS_REFRESH: bool;
}

impl<Key: 'static, Request: 'static, Response: 'static> ActivationPolicy
    for Read<Key, Request, Response>
{
    const REQUIRES_TIMEOUT: bool = true;
    const ALLOWS_REFRESH: bool = true;
}

impl<Key: 'static, RequestBody: 'static, Response: 'static> ActivationPolicy
    for Request<Key, RequestBody, Response>
{
    const REQUIRES_TIMEOUT: bool = true;
    const ALLOWS_REFRESH: bool = false;
}

impl<Key: 'static, Response: 'static> ActivationPolicy for Operation<Key, Response> {
    const REQUIRES_TIMEOUT: bool = false;
    const ALLOWS_REFRESH: bool = false;
}

/// A latest snapshot with explicit absence.
pub struct Latest<T> {
    value: Option<Sample<T>>,
}

impl<T> Latest<T> {
    /// Creates an unavailable snapshot.
    #[must_use]
    pub const fn unavailable() -> Self {
        Self { value: None }
    }

    /// Creates an available snapshot with its original observation stamp.
    #[must_use]
    pub fn new(value: T, stamp: ObservationStamp) -> Self {
        Self {
            value: Some(Sample::new(value, stamp)),
        }
    }

    /// Creates a snapshot from an already stamped observation.
    #[must_use]
    pub fn from_sample(sample: Sample<T>) -> Self {
        Self {
            value: Some(sample),
        }
    }

    /// Returns the captured snapshot, if one was admitted.
    #[must_use]
    pub fn sample(&self) -> Option<&Sample<T>> {
        self.value.as_ref()
    }

    /// Returns the payload, if available.
    #[must_use]
    pub fn value(&self) -> Option<&T> {
        self.value.as_ref().map(Sample::payload)
    }

    /// Reports whether the snapshot can satisfy an age bound at `now`.
    #[must_use]
    pub fn is_fresh_at(&self, now: ExecutionTime, max_age_ms: Option<u64>) -> bool {
        let Some(sample) = &self.value else {
            return false;
        };
        let Some(age) = now.checked_duration_since(sample.stamp().capture_time()) else {
            return false;
        };
        max_age_ms.is_none_or(|limit| age.as_millis() <= limit)
    }

    /// Consumes the input and returns its stamped payload.
    pub fn into_sample(self) -> Option<Sample<T>> {
        self.value
    }
}

impl<T> Default for Latest<T> {
    fn default() -> Self {
        Self::unavailable()
    }
}

impl<T: 'static> InputSpec for Latest<T> {
    const KIND: InputKind = InputKind::Latest;
}

/// A bounded ordered batch of captured observations.
pub struct Samples<T> {
    items: Vec<Sample<T>>,
    gap: bool,
}

impl<T> Samples<T> {
    /// Creates a complete batch.
    #[must_use]
    pub fn new(items: Vec<Sample<T>>) -> Self {
        Self { items, gap: false }
    }

    /// Creates a retained prefix whose source reported a gap.
    #[must_use]
    pub fn with_gap(items: Vec<Sample<T>>) -> Self {
        Self { items, gap: true }
    }

    /// Creates a batch after checking its complete item and byte bounds.
    pub fn bounded(
        items: Vec<Sample<T>>,
        encoded_bytes: u64,
        capacity: Capacity,
    ) -> Result<Self, CapacityError> {
        capacity.check(items.len() as u64, encoded_bytes)?;
        Ok(Self::new(items))
    }

    /// Returns the retained ordered prefix.
    #[must_use]
    pub fn items(&self) -> &[Sample<T>] {
        &self.items
    }

    /// Reports whether records were lost before this retained prefix.
    #[must_use]
    pub const fn has_gap(&self) -> bool {
        self.gap
    }

    /// Consumes the batch.
    #[must_use]
    pub fn into_items(self) -> Vec<Sample<T>> {
        self.items
    }
}

impl<T> Default for Samples<T> {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl<T: 'static> InputSpec for Samples<T> {
    const KIND: InputKind = InputKind::Samples;
}

/// A bounded ordered batch of discrete occurrences.
pub struct Events<T> {
    items: Vec<T>,
    gap: bool,
}

impl<T> Events<T> {
    /// Creates a complete batch.
    #[must_use]
    pub fn new(items: Vec<T>) -> Self {
        Self { items, gap: false }
    }

    /// Creates a retained prefix whose source reported a gap.
    #[must_use]
    pub fn with_gap(items: Vec<T>) -> Self {
        Self { items, gap: true }
    }

    /// Creates a batch after checking its complete item and byte bounds.
    pub fn bounded(
        items: Vec<T>,
        encoded_bytes: u64,
        capacity: Capacity,
    ) -> Result<Self, CapacityError> {
        capacity.check(items.len() as u64, encoded_bytes)?;
        Ok(Self::new(items))
    }

    /// Returns the ordered occurrences.
    #[must_use]
    pub fn items(&self) -> &[T] {
        &self.items
    }

    /// Reports whether records were lost before this retained prefix.
    #[must_use]
    pub const fn has_gap(&self) -> bool {
        self.gap
    }

    /// Consumes the batch.
    #[must_use]
    pub fn into_items(self) -> Vec<T> {
        self.items
    }
}

impl<T> Default for Events<T> {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl<T: 'static> InputSpec for Events<T> {
    const KIND: InputKind = InputKind::Events;
}

/// A replaceable intent with an explicit validity interval.
pub struct Setpoint<T> {
    value: Option<T>,
    issued_at: Option<ExecutionTime>,
    valid_until: Option<ExecutionTime>,
}

impl<T> Setpoint<T> {
    /// Creates an explicit withdrawal.
    #[must_use]
    pub const fn withdrawn() -> Self {
        Self {
            value: None,
            issued_at: None,
            valid_until: None,
        }
    }

    /// Creates an intent valid for the supplied duration.
    #[must_use]
    pub fn new(value: T, issued_at: ExecutionTime, valid_for_ms: u64) -> Self {
        let valid_until = issued_at.checked_add_millis(valid_for_ms);
        Self {
            value: Some(value),
            issued_at: Some(issued_at),
            valid_until,
        }
    }

    /// Returns the current value when the intent is present.
    #[must_use]
    pub fn value(&self) -> Option<&T> {
        self.value.as_ref()
    }

    /// Returns the issue instant.
    #[must_use]
    pub const fn issued_at(&self) -> Option<ExecutionTime> {
        self.issued_at
    }

    /// Reports whether the intent is valid at the supplied instant.
    #[must_use]
    pub const fn is_valid_at(&self, now: ExecutionTime) -> bool {
        match (self.value.as_ref(), self.valid_until) {
            (Some(_), Some(until)) => now.as_nanos() <= until.as_nanos(),
            _ => false,
        }
    }
}

impl<T> Default for Setpoint<T> {
    fn default() -> Self {
        Self::withdrawn()
    }
}

impl<T: 'static> InputSpec for Setpoint<T> {
    const KIND: InputKind = InputKind::Setpoint;
}

/// An ordered stream batch with explicit continuity and terminal records.
pub struct Stream<T> {
    items: Vec<StreamItem<T>>,
}

impl<T> Stream<T> {
    /// Creates a stream batch.
    #[must_use]
    pub fn new(items: Vec<StreamItem<T>>) -> Self {
        Self { items }
    }

    /// Creates a stream batch after checking its complete item and byte
    /// bounds, including terminal control records.
    pub fn bounded(
        items: Vec<StreamItem<T>>,
        encoded_bytes: u64,
        capacity: Capacity,
    ) -> Result<Self, CapacityError> {
        capacity.check(items.len() as u64, encoded_bytes)?;
        Ok(Self::new(items))
    }

    /// Returns the ordered stream records.
    #[must_use]
    pub fn items(&self) -> &[StreamItem<T>] {
        &self.items
    }

    /// Consumes the stream batch.
    #[must_use]
    pub fn into_items(self) -> Vec<StreamItem<T>> {
        self.items
    }
}

impl<T> Default for Stream<T> {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl<T: 'static> InputSpec for Stream<T> {
    const KIND: InputKind = InputKind::Stream;
}

/// A source failure attached to a terminal stream record.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct StreamFailure {
    reason: String,
}

impl StreamFailure {
    /// Creates a diagnostic failure reason.
    #[must_use]
    pub fn new(reason: impl Into<String>) -> Self {
        Self {
            reason: reason.into(),
        }
    }

    /// Returns the diagnostic reason.
    #[must_use]
    pub fn reason(&self) -> &str {
        &self.reason
    }
}

/// One stream record.
pub enum StreamItem<T> {
    /// A captured data item.
    Data(Sample<T>),
    /// A continuity gap before a later retained record.
    Gap,
    /// Normal end of the stream timeline.
    End,
    /// Terminal source failure.
    Failed(StreamFailure),
}

impl<T: fmt::Debug> fmt::Debug for StreamItem<T> {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Data(value) => formatter.debug_tuple("Data").field(value).finish(),
            Self::Gap => formatter.write_str("Gap"),
            Self::End => formatter.write_str("End"),
            Self::Failed(value) => formatter.debug_tuple("Failed").field(value).finish(),
        }
    }
}

/// A unique correlation assigned at Commands queue admission.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CommandId(u64);

impl CommandId {
    /// Creates a command correlation from a target sequence.
    #[must_use]
    pub const fn new(sequence: u64) -> Self {
        Self(sequence)
    }

    /// Returns the target sequence.
    #[must_use]
    pub const fn sequence(self) -> u64 {
        self.0
    }
}

/// The deterministic merge key for one admitted command.
#[derive(Clone, Copy, Debug, Eq, Hash, Ord, PartialEq, PartialOrd)]
pub struct CommandOrder {
    eligible_boundary: u64,
    caller_rank: u64,
    sequence: CommandId,
}

impl CommandOrder {
    /// Creates an order key from eligible boundary, caller rank, and source
    /// queue sequence.
    #[must_use]
    pub const fn new(eligible_boundary: u64, caller_rank: u64, sequence: CommandId) -> Self {
        Self {
            eligible_boundary,
            caller_rank,
            sequence,
        }
    }

    /// Returns the first boundary at which the command may be selected.
    #[must_use]
    pub const fn eligible_boundary(self) -> u64 {
        self.eligible_boundary
    }

    /// Returns the stable lexical caller rank.
    #[must_use]
    pub const fn caller_rank(self) -> u64 {
        self.caller_rank
    }

    /// Returns the originating queue sequence.
    #[must_use]
    pub const fn sequence(self) -> CommandId {
        self.sequence
    }
}

/// A command order violation detected before selection.
#[derive(Clone, Copy, Debug, Eq, PartialEq, thiserror::Error)]
pub enum CommandOrderError {
    /// The frozen command list is not in deterministic merge order.
    #[error("commands are not in deterministic admission order")]
    OutOfOrder {
        /// Earlier order key.
        previous: CommandOrder,
        /// Later order key that sorts before it.
        current: CommandOrder,
    },
}

/// One admitted behavioral request and its typed response family.
pub struct Command<Request, Response> {
    order: CommandOrder,
    request: Request,
    response: PhantomData<fn() -> Response>,
}

impl<Request, Response> Command<Request, Response> {
    /// Creates an admitted command.
    #[must_use]
    pub fn new(id: CommandId, request: Request) -> Self {
        Self {
            order: CommandOrder::new(0, 0, id),
            request,
            response: PhantomData,
        }
    }

    /// Creates an admitted command with an explicit deterministic order key.
    #[must_use]
    pub fn with_order(order: CommandOrder, request: Request) -> Self {
        Self {
            order,
            request,
            response: PhantomData,
        }
    }

    /// Returns the correlation identity.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        self.order.sequence()
    }

    /// Returns the complete deterministic merge key.
    #[must_use]
    pub const fn order(&self) -> CommandOrder {
        self.order
    }

    /// Returns the immutable request payload.
    #[must_use]
    pub fn request(&self) -> &Request {
        &self.request
    }

    /// Creates a typed processing reply for this command.
    #[must_use]
    pub fn reply(&self, response: Response) -> Reply<Response> {
        Reply {
            id: self.id(),
            response,
        }
    }
}

/// One correlated response for an admitted command.
pub struct Reply<Response> {
    id: CommandId,
    response: Response,
}

impl<Response> Reply<Response> {
    /// Creates a reply directly for an admitted command id.
    #[must_use]
    pub fn new(id: CommandId, response: Response) -> Self {
        Self { id, response }
    }

    /// Returns the command correlation.
    #[must_use]
    pub const fn id(&self) -> CommandId {
        self.id
    }

    /// Returns the response payload.
    #[must_use]
    pub fn response(&self) -> &Response {
        &self.response
    }

    /// Consumes the reply.
    #[must_use]
    pub fn into_response(self) -> Response {
        self.response
    }
}

/// An ordered Commands input batch.
pub struct Commands<Request, Response> {
    items: Vec<Command<Request, Response>>,
}

impl<Request, Response> Commands<Request, Response> {
    /// Creates an ordered batch.
    #[must_use]
    pub fn new(items: Vec<Command<Request, Response>>) -> Self {
        Self { items }
    }

    /// Creates an ordered batch after checking its complete item and byte
    /// bounds.
    pub fn bounded(
        items: Vec<Command<Request, Response>>,
        encoded_bytes: u64,
        capacity: Capacity,
    ) -> Result<Self, CapacityError> {
        capacity.check(items.len() as u64, encoded_bytes)?;
        Ok(Self::new(items))
    }

    /// Returns commands in queue-admission order.
    #[must_use]
    pub fn items(&self) -> &[Command<Request, Response>] {
        &self.items
    }

    /// Consumes the batch.
    #[must_use]
    pub fn into_items(self) -> Vec<Command<Request, Response>> {
        self.items
    }

    /// Verifies that the frozen batch is in deterministic merge order.
    pub fn validate_order(&self) -> Result<(), CommandOrderError> {
        for pair in self.items.windows(2) {
            let previous = pair[0].order();
            let current = pair[1].order();
            if current < previous {
                return Err(CommandOrderError::OutOfOrder { previous, current });
            }
        }
        Ok(())
    }
}

impl<Request, Response> Default for Commands<Request, Response> {
    fn default() -> Self {
        Self::new(Vec::new())
    }
}

impl<Request: 'static, Response: 'static> InputSpec for Commands<Request, Response> {
    const KIND: InputKind = InputKind::Commands;
}

/// Exchange status for a keyed immutable read.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum ReadStatus {
    /// No activation is currently selected.
    Inactive,
    /// An activation is selected but no completion has been admitted.
    Pending,
    /// A completion or retained value is available for the current key.
    Completed,
}

/// A typed failure of a read exchange.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum ReadError {
    /// The request was not transmitted.
    #[error("read was not sent: {0}")]
    NotSent(String),
    /// Transmission may have occurred without definitive result evidence.
    #[error("read outcome is unknown: {0}")]
    OutcomeUnknown(String),
    /// The remote read exceeded its host deadline.
    #[error("read timed out")]
    Timeout,
    /// The requested revision or endpoint is unavailable.
    #[error("read is unavailable: {0}")]
    Unavailable(String),
    /// The response exceeded the declared bound.
    #[error("read response exceeded its byte bound")]
    Oversized,
    /// The transport reported a failure.
    #[error("read transport failed: {0}")]
    Transport(String),
}

/// The first admitted completion for one read attempt.
pub struct ReadCompletion<Key, Response> {
    key: Key,
    result: Result<Response, ReadError>,
}

impl<Key, Response> ReadCompletion<Key, Response> {
    /// Creates a successful completion.
    #[must_use]
    pub fn success(key: Key, response: Response) -> Self {
        Self {
            key,
            result: Ok(response),
        }
    }

    /// Creates a failed completion.
    #[must_use]
    pub fn failure(key: Key, error: ReadError) -> Self {
        Self {
            key,
            result: Err(error),
        }
    }

    /// Returns the completion key.
    #[must_use]
    pub fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the typed response or transport failure.
    pub fn result(&self) -> Result<&Response, &ReadError> {
        self.result.as_ref()
    }

    /// Consumes the completion result.
    pub fn into_result(self) -> Result<Response, ReadError> {
        self.result
    }

    /// Consumes the completion into its key and result.
    pub fn into_parts(self) -> (Key, Result<Response, ReadError>) {
        (self.key, self.result)
    }
}

/// A retained successful read for the current key.
pub struct ReadSuccess<Key, Response> {
    key: Key,
    response: Response,
    provenance: ObservationStamp,
}

impl<Key, Response> ReadSuccess<Key, Response> {
    /// Creates retained success evidence.
    #[must_use]
    pub fn new(key: Key, response: Response, provenance: ObservationStamp) -> Self {
        Self {
            key,
            response,
            provenance,
        }
    }

    /// Returns the current activation key.
    #[must_use]
    pub fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the retained response.
    #[must_use]
    pub fn response(&self) -> &Response {
        &self.response
    }

    /// Returns source and capture metadata.
    #[must_use]
    pub fn provenance(&self) -> &ObservationStamp {
        &self.provenance
    }

    /// Consumes retained success evidence.
    #[must_use]
    pub fn into_parts(self) -> (Key, Response, ObservationStamp) {
        (self.key, self.response, self.provenance)
    }
}

/// A keyed immutable read input snapshot.
pub struct Read<Key, Request, Response> {
    status: ReadStatus,
    key: Option<Key>,
    completion: Option<ReadCompletion<Key, Response>>,
    retained_success: Option<ReadSuccess<Key, Response>>,
    request: PhantomData<fn() -> Request>,
}

impl<Key, Request, Response> Read<Key, Request, Response> {
    /// Creates an inactive read.
    #[must_use]
    pub const fn inactive() -> Self {
        Self {
            status: ReadStatus::Inactive,
            key: None,
            completion: None,
            retained_success: None,
            request: PhantomData,
        }
    }

    /// Creates a pending read for an owned activation key.
    #[must_use]
    pub fn pending(key: Key) -> Self {
        Self {
            status: ReadStatus::Pending,
            key: Some(key),
            completion: None,
            retained_success: None,
            request: PhantomData,
        }
    }

    /// Creates a completed read input.
    #[must_use]
    pub fn completed(key: Key, result: Result<Response, ReadError>) -> Self {
        let completion = ReadCompletion { key, result };
        let key = None;
        Self {
            status: ReadStatus::Completed,
            key,
            completion: Some(completion),
            retained_success: None,
            request: PhantomData,
        }
    }

    /// Adds historical same-key success while retaining a new completion.
    #[must_use]
    pub fn with_retained_success(mut self, success: ReadSuccess<Key, Response>) -> Self {
        self.retained_success = Some(success);
        self
    }

    /// Returns the current exchange status.
    #[must_use]
    pub const fn status(&self) -> ReadStatus {
        self.status
    }

    /// Returns the current key when active.
    #[must_use]
    pub fn key(&self) -> Option<&Key> {
        self.key.as_ref().or_else(|| {
            self.completion
                .as_ref()
                .map(ReadCompletion::key)
                .or_else(|| self.retained_success.as_ref().map(ReadSuccess::key))
        })
    }

    /// Returns a completion only in the invocation that first admits it.
    #[must_use]
    pub fn new_completion(&self) -> Option<&ReadCompletion<Key, Response>> {
        self.completion.as_ref()
    }

    /// Returns retained same-key success, including during a refresh.
    #[must_use]
    pub fn retained_success(&self) -> Option<&ReadSuccess<Key, Response>> {
        self.retained_success.as_ref()
    }
}

impl<Key, Request, Response> Default for Read<Key, Request, Response> {
    fn default() -> Self {
        Self::inactive()
    }
}

impl<Key: 'static, Request: 'static, Response: 'static> InputSpec for Read<Key, Request, Response> {
    const KIND: InputKind = InputKind::Read;
}

/// A typed request outcome.  Unlike Read, a timeout may leave remote effects
/// uncertain and must never be replayed implicitly.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum RequestError {
    /// A definite local failure before transmission.
    #[error("request was not sent: {0}")]
    NotSent(String),
    /// The request may have reached its target without a definitive response.
    #[error("request outcome is unknown: {0}")]
    OutcomeUnknown(String),
    /// The target rejected the request before queue admission.
    #[error("request rejected before admission: {0}")]
    RejectedBeforeAdmission(String),
    /// The request response exceeded its declared bound.
    #[error("request response exceeded its byte bound")]
    Oversized,
    /// The request timed out before response evidence was available.
    #[error("request timed out")]
    Timeout,
}

/// A first admitted completion for a keyed request.
pub struct RequestCompletion<Key, Response> {
    key: Key,
    result: Result<Response, RequestError>,
}

impl<Key, Response> RequestCompletion<Key, Response> {
    /// Creates a request completion.
    #[must_use]
    pub fn new(key: Key, result: Result<Response, RequestError>) -> Self {
        Self { key, result }
    }

    /// Returns the request key.
    #[must_use]
    pub fn key(&self) -> &Key {
        &self.key
    }

    /// Returns response or uncertainty evidence.
    pub fn result(&self) -> Result<&Response, &RequestError> {
        self.result.as_ref()
    }

    /// Consumes the completion into its key and result.
    pub fn into_parts(self) -> (Key, Result<Response, RequestError>) {
        (self.key, self.result)
    }
}

/// A keyed request input snapshot.
pub struct Request<Key, RequestBody, Response> {
    status: ReadStatus,
    key: Option<Key>,
    completion: Option<RequestCompletion<Key, Response>>,
    request: PhantomData<fn() -> RequestBody>,
}

impl<Key, RequestBody, Response> Request<Key, RequestBody, Response> {
    /// Creates an inactive request input.
    #[must_use]
    pub const fn inactive() -> Self {
        Self {
            status: ReadStatus::Inactive,
            key: None,
            completion: None,
            request: PhantomData,
        }
    }

    /// Creates a pending request input.
    #[must_use]
    pub const fn pending() -> Self {
        Self {
            status: ReadStatus::Pending,
            key: None,
            completion: None,
            request: PhantomData,
        }
    }

    /// Creates a pending request for a selected key.
    #[must_use]
    pub fn pending_for(key: Key) -> Self {
        Self {
            status: ReadStatus::Pending,
            key: Some(key),
            completion: None,
            request: PhantomData,
        }
    }

    /// Creates a request completion input.
    #[must_use]
    pub fn completed(key: Key, result: Result<Response, RequestError>) -> Self {
        Self {
            status: ReadStatus::Completed,
            key: None,
            completion: Some(RequestCompletion { key, result }),
            request: PhantomData,
        }
    }

    /// Returns the exchange status.
    #[must_use]
    pub const fn status(&self) -> ReadStatus {
        self.status
    }

    /// Returns the selected key, if active.
    #[must_use]
    pub fn key(&self) -> Option<&Key> {
        self.key
            .as_ref()
            .or_else(|| self.completion.as_ref().map(RequestCompletion::key))
    }

    /// Returns the newly admitted completion.
    #[must_use]
    pub fn new_completion(&self) -> Option<&RequestCompletion<Key, Response>> {
        self.completion.as_ref()
    }
}

impl<Key, RequestBody, Response> Default for Request<Key, RequestBody, Response> {
    fn default() -> Self {
        Self::inactive()
    }
}

impl<Key: 'static, RequestBody: 'static, Response: 'static> InputSpec
    for Request<Key, RequestBody, Response>
{
    const KIND: InputKind = InputKind::Request;
}

/// A typed failure from a local finite operation.
#[derive(Clone, Debug, Eq, PartialEq, thiserror::Error)]
pub enum OperationInputError {
    /// The worker failed with a typed diagnostic.
    #[error("operation failed: {0}")]
    Failed(String),
    /// The worker exceeded its deadline and was retired.
    #[error("operation timed out")]
    Timeout,
    /// The worker completion was no longer current.
    #[error("operation completion is stale")]
    Stale,
}

/// One admitted operation completion.
pub struct OperationCompletion<Key, Response> {
    key: Key,
    result: Result<Response, OperationInputError>,
}

impl<Key, Response> OperationCompletion<Key, Response> {
    /// Creates an operation completion.
    #[must_use]
    pub fn new(key: Key, result: Result<Response, OperationInputError>) -> Self {
        Self { key, result }
    }

    /// Returns the operation key.
    #[must_use]
    pub fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the result or typed failure.
    pub fn result(&self) -> Result<&Response, &OperationInputError> {
        self.result.as_ref()
    }

    /// Consumes the completion into its key and result.
    pub fn into_parts(self) -> (Key, Result<Response, OperationInputError>) {
        (self.key, self.result)
    }
}

/// A keyed local operation input snapshot.
pub struct Operation<Key, Response> {
    status: ReadStatus,
    key: Option<Key>,
    completion: Option<OperationCompletion<Key, Response>>,
}

impl<Key, Response> Operation<Key, Response> {
    /// Creates an inactive operation input.
    #[must_use]
    pub const fn inactive() -> Self {
        Self {
            status: ReadStatus::Inactive,
            key: None,
            completion: None,
        }
    }

    /// Creates a pending operation input.
    #[must_use]
    pub const fn pending() -> Self {
        Self {
            status: ReadStatus::Pending,
            key: None,
            completion: None,
        }
    }

    /// Creates a pending operation for a selected key.
    #[must_use]
    pub fn pending_for(key: Key) -> Self {
        Self {
            status: ReadStatus::Pending,
            key: Some(key),
            completion: None,
        }
    }

    /// Creates an admitted operation completion.
    #[must_use]
    pub fn completed(key: Key, result: Result<Response, OperationInputError>) -> Self {
        Self {
            status: ReadStatus::Completed,
            key: None,
            completion: Some(OperationCompletion { key, result }),
        }
    }

    /// Returns the current status.
    #[must_use]
    pub const fn status(&self) -> ReadStatus {
        self.status
    }

    /// Returns the selected key, if active.
    #[must_use]
    pub fn key(&self) -> Option<&Key> {
        self.key
            .as_ref()
            .or_else(|| self.completion.as_ref().map(OperationCompletion::key))
    }

    /// Returns the newly admitted completion.
    #[must_use]
    pub fn new_completion(&self) -> Option<&OperationCompletion<Key, Response>> {
        self.completion.as_ref()
    }
}

impl<Key, Response> Default for Operation<Key, Response> {
    fn default() -> Self {
        Self::inactive()
    }
}

impl<Key: 'static, Response: 'static> InputSpec for Operation<Key, Response> {
    const KIND: InputKind = InputKind::Operation;
}

/// An owned operation activation selected by a runtime projection.
pub struct Activation<Key, Input> {
    key: Key,
    input: Input,
}

impl<Key, Input> Activation<Key, Input> {
    /// Creates an activation without dispatching work.
    #[must_use]
    pub fn new(key: Key, input: Input) -> Self {
        Self { key, input }
    }

    /// Returns the activation key.
    #[must_use]
    pub fn key(&self) -> &Key {
        &self.key
    }

    /// Returns the owned worker input.
    #[must_use]
    pub fn input(&self) -> &Input {
        &self.input
    }

    /// Consumes the activation into its key and input.
    #[must_use]
    pub fn into_parts(self) -> (Key, Input) {
        (self.key, self.input)
    }
}

impl ExecutionTime {
    pub(crate) fn checked_add_millis(self, milliseconds: u64) -> Option<Self> {
        milliseconds
            .checked_mul(1_000_000)
            .and_then(|nanos| self.as_nanos().checked_add(nanos))
            .map(Self::from_nanos)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        Activation, Capacity, CapacityError, Command, CommandId, CommandOrder, CommandOrderError,
        Commands, InputKind, InputSpec, Read, ReadError, ReadStatus, Samples,
    };
    use crate::runtime::{ExecutionTime, ObservationStamp, Sample};

    #[test]
    fn command_reply_preserves_queue_admission_identity() {
        let commands = Commands::new(vec![Command::<u8, u16>::new(CommandId::new(7), 3)]);
        let reply = commands.items()[0].reply(11);
        assert_eq!(reply.id().sequence(), 7);
        assert_eq!(*reply.response(), 11);
    }

    #[test]
    fn read_separates_new_failure_from_inactive_status() {
        let read = Read::<u8, (), u16>::completed(4, Err(ReadError::Timeout));
        assert_eq!(read.status(), ReadStatus::Completed);
        assert_eq!(read.new_completion().expect("completion").key(), &4);
        assert!(read.new_completion().expect("completion").result().is_err());
    }

    #[test]
    fn snapshot_inputs_preserve_stamp_and_gap() {
        let stamp = ObservationStamp::new("imu", ExecutionTime::from_nanos(10), Some(2));
        let samples = Samples::with_gap(vec![Sample::new(1_u8, stamp)]);
        assert!(samples.has_gap());
        assert_eq!(samples.items()[0].stamp().revision(), Some(2));
        assert_eq!(<Samples<u8> as InputSpec>::KIND, InputKind::Samples);
    }

    #[test]
    fn activation_owns_key_and_input_until_dispatch() {
        let activation = Activation::new(String::from("goal"), vec![1_u8, 2]);
        let (key, input) = activation.into_parts();
        assert_eq!(key, "goal");
        assert_eq!(input, [1, 2]);
    }

    #[test]
    fn command_order_is_lexical_and_capacity_is_whole_batch() {
        let capacity = Capacity::new(2, 8).expect("valid capacity");
        assert!(capacity.check(2, 8).is_ok());
        assert_eq!(
            capacity.check(3, 1),
            Err(CapacityError::ItemsExceeded {
                limit: 2,
                actual: 3
            })
        );

        let first: Command<u8, u16> =
            Command::with_order(CommandOrder::new(1, 2, CommandId::new(1)), 1_u8);
        let second: Command<u8, u16> =
            Command::with_order(CommandOrder::new(1, 1, CommandId::new(2)), 2_u8);
        let commands = Commands::new(vec![first, second]);
        assert_eq!(
            commands.validate_order(),
            Err(CommandOrderError::OutOfOrder {
                previous: CommandOrder::new(1, 2, CommandId::new(1)),
                current: CommandOrder::new(1, 1, CommandId::new(2)),
            })
        );
    }
}
