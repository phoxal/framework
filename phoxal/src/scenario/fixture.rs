//! Function-based simulation fixture used by ordinary Rust tests.

use std::marker::PhantomData;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use prost::Message;

use super::{Action, CapturePolicy, CaptureRecord, CommandReply, NativeBodySample, Step, Validity};
use crate::port::{PortKind, PortSignature};

static NEXT_PLAN_ID: AtomicU64 = AtomicU64::new(1);

/// A simulation fixture scoped to one Rust test invocation.
#[derive(Debug)]
pub struct Simulation {
    test_identity: String,
    scene: PathBuf,
}

impl Simulation {
    /// Construct a fixture for an ordinary `#[test]` without using the
    /// `#[phoxal::scenario]` convenience attribute.
    pub fn from_context(test_identity: impl Into<String>) -> crate::Result<Self> {
        Self::from_host_context(test_identity)
    }

    /// Construct the fixture from the immutable context installed by
    /// `cargo phoxal test`.
    pub fn from_host_context(test_identity: impl Into<String>) -> crate::Result<Self> {
        let test_identity = test_identity.into();
        if test_identity.trim().is_empty() {
            return Err(crate::anyhow!("simulation test identity must not be empty"));
        }
        let scene = std::env::var_os(super::fixture_protocol::ENV_SCENE)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("simulation/scene.xml"));
        Ok(Self {
            test_identity,
            scene,
        })
    }

    /// Select another scene for this test. Relative paths are resolved by the
    /// run host against the owning robot project.
    #[must_use]
    pub fn with_scene(mut self, scene: impl Into<PathBuf>) -> Self {
        self.scene = scene.into();
        self
    }

    /// Begin one finite experiment.
    pub fn plan(&mut self) -> Plan {
        Plan::new()
    }

    /// Execute one finite plan and return only validated, finalized evidence.
    pub fn run(&mut self, plan: Plan) -> crate::Result<CompletedRun> {
        super::fixture_protocol::run(&self.test_identity, &self.scene, plan)
    }
}

/// One finite experiment under construction.
#[derive(Debug)]
pub struct Plan {
    pub(crate) id: u64,
    cursor: u32,
    steps: Vec<Step>,
    captures: Vec<super::Capture>,
    next_action: u32,
    next_capture: u32,
}

impl Plan {
    fn new() -> Self {
        Self {
            id: NEXT_PLAN_ID.fetch_add(1, Ordering::Relaxed),
            cursor: 0,
            steps: Vec::new(),
            captures: Vec::new(),
            next_action: 0,
            next_capture: 0,
        }
    }

    /// Stage one generated call, lease update, or withdrawal at the current
    /// logical boundary. No transition is appended implicitly.
    pub fn send<O>(&mut self, operation: O) -> crate::Result<ReplyTicket<O::Response>>
    where
        O: SendOperation,
    {
        let index = self.next_action;
        self.next_action = self
            .next_action
            .checked_add(1)
            .ok_or_else(|| crate::anyhow!("scenario action count overflowed"))?;
        let label = format!("action-{index}");
        let action = operation.into_action(&label)?;
        self.steps.push(Step::new(label, self.cursor, action));
        Ok(ReplyTicket {
            plan_id: self.id,
            index,
            marker: PhantomData,
        })
    }

    /// Declare one typed observation capture before execution.
    pub fn record<O>(
        &mut self,
        observation: O,
        policy: CapturePolicy,
    ) -> crate::Result<Capture<O::Value>>
    where
        O: ObservationOperation,
    {
        let index = self.next_capture;
        self.next_capture = self
            .next_capture
            .checked_add(1)
            .ok_or_else(|| crate::anyhow!("scenario capture count overflowed"))?;
        let fallback_name = format!("capture-{index}");
        let capture = observation.into_capture(&fallback_name, policy)?;
        let name = capture_name(&capture).to_owned();
        self.captures.push(capture);
        Ok(Capture {
            plan_id: self.id,
            name,
            policy: Some(policy),
            encoding: CaptureEncoding::Protobuf,
            marker: PhantomData,
        })
    }

    /// Record native truth for one named simulator body.
    pub fn record_body(
        &mut self,
        body: impl Into<String>,
    ) -> crate::Result<Capture<NativeBodySample>> {
        self.next_capture = self
            .next_capture
            .checked_add(1)
            .ok_or_else(|| crate::anyhow!("scenario capture count overflowed"))?;
        let name = body.into();
        self.captures.push(
            super::Capture::native_body(name.clone(), "SI", "world")
                .map_err(|error| crate::anyhow!("native body capture: {error}"))?,
        );
        Ok(Capture {
            plan_id: self.id,
            name,
            policy: None,
            encoding: CaptureEncoding::NativeBodyJson,
            marker: PhantomData,
        })
    }

    /// Advance the authored cursor by exactly `steps` native transitions.
    pub fn wait_steps(&mut self, steps: u32) -> crate::Result<()> {
        self.cursor = self
            .cursor
            .checked_add(steps)
            .ok_or_else(|| crate::anyhow!("scenario transition count overflowed"))?;
        Ok(())
    }

    pub(crate) fn compile(
        self,
        scene: PathBuf,
        quantum: super::Quantum,
        name: &str,
    ) -> crate::Result<(super::Program, u64)> {
        if self.cursor == 0 {
            return Err(crate::anyhow!(
                "scenario plan must contain at least one transition"
            ));
        }
        if let Some(step) = self.steps.iter().find(|step| step.boundary >= self.cursor) {
            return Err(crate::anyhow!(
                "scenario action `{}` is at terminal boundary {}; add the intended remaining simulation time with wait_steps",
                step.label,
                self.cursor,
            ));
        }
        let nanos = u64::from(quantum.micros())
            .checked_mul(1_000)
            .and_then(|value| value.checked_mul(u64::from(self.cursor)))
            .ok_or_else(|| crate::anyhow!("scenario duration overflowed"))?;
        let _ = scene;
        let steps = expand_lease_renewals(self.steps, self.cursor, quantum)?;
        let schedule = steps
            .into_iter()
            .map(|step| super::ScheduleEntry::at(step.boundary, step.action))
            .collect();
        let program = super::Program::normalize(
            name,
            quantum,
            Duration::from_nanos(nanos),
            schedule,
            self.captures,
        )
        .map_err(|error| crate::anyhow!("scenario program: {error}"))?;
        Ok((program, self.id))
    }
}

fn expand_lease_renewals(
    mut steps: Vec<Step>,
    terminal_boundary: u32,
    quantum: super::Quantum,
) -> crate::Result<Vec<Step>> {
    let authored = steps.clone();
    let quantum_ns = u64::from(quantum.micros())
        .checked_mul(1_000)
        .ok_or_else(|| crate::anyhow!("scenario quantum overflowed"))?;
    for step in &authored {
        let Action::Setpoint {
            target_instance,
            consumer_signature,
            encoded_payload,
            validity: Validity::Lease { valid_for_ms },
        } = &step.action
        else {
            continue;
        };
        let lease_ns = valid_for_ms
            .checked_mul(1_000_000)
            .ok_or_else(|| crate::anyhow!("scenario lease interval overflowed"))?;
        if lease_ns < quantum_ns {
            return Err(crate::anyhow!(
                "scenario lease for {}.{} is shorter than one native quantum",
                target_instance,
                consumer_signature.name,
            ));
        }
        let lease_steps = lease_ns / quantum_ns;
        let renewal_stride = u32::try_from((lease_steps / 2).max(1))
            .map_err(|_| crate::anyhow!("scenario lease renewal stride overflowed"))?;
        let replacement = authored
            .iter()
            .filter(|candidate| candidate.boundary > step.boundary)
            .filter(|candidate| {
                action_replaces_lease(&candidate.action, target_instance, consumer_signature)
            })
            .map(|candidate| candidate.boundary)
            .min()
            .unwrap_or(terminal_boundary);
        let mut boundary = step
            .boundary
            .checked_add(renewal_stride)
            .ok_or_else(|| crate::anyhow!("scenario lease renewal boundary overflowed"))?;
        let mut renewal = 0_u32;
        while boundary < replacement && boundary < terminal_boundary {
            steps.push(Step::new(
                format!("{}-renew-{renewal}", step.label),
                boundary,
                Action::Setpoint {
                    target_instance: target_instance.clone(),
                    consumer_signature: *consumer_signature,
                    encoded_payload: encoded_payload.clone(),
                    validity: Validity::Lease {
                        valid_for_ms: *valid_for_ms,
                    },
                },
            ));
            renewal = renewal
                .checked_add(1)
                .ok_or_else(|| crate::anyhow!("scenario lease renewal count overflowed"))?;
            boundary = boundary
                .checked_add(renewal_stride)
                .ok_or_else(|| crate::anyhow!("scenario lease renewal boundary overflowed"))?;
        }
    }
    Ok(steps)
}

fn action_replaces_lease(
    action: &Action,
    target_instance: &str,
    signature: &PortSignature,
) -> bool {
    match action {
        Action::Setpoint {
            target_instance: target,
            consumer_signature,
            ..
        } => target == target_instance && consumer_signature == signature,
        Action::Withdraw {
            target_instance: target,
            producer_signature,
        } => target == target_instance && producer_signature == signature,
        Action::Command { .. } => false,
    }
}

/// Generated operation accepted by [`Plan::send`].
pub trait SendOperation {
    /// Typed response associated with this operation.
    type Response: ScenarioValue;

    /// Convert generated operation metadata into the finite execution action.
    fn into_action(self, label: &str) -> crate::Result<Action>;
}

/// Generated observation accepted by [`Plan::record`].
pub trait ObservationOperation {
    /// Typed value published by this observation.
    type Value: ScenarioValue;

    /// Convert generated observation metadata into a capture declaration.
    fn into_capture(self, name: &str, policy: CapturePolicy) -> crate::Result<super::Capture>;
}

impl<Request, Response> SendOperation for crate::contract::Call<Request, Response>
where
    Request: Message,
    Response: ScenarioValue,
{
    type Response = Response;

    fn into_action(self, label: &str) -> crate::Result<Action> {
        let (instance, signature, request) = self.into_parts();
        if let Some(lease) = signature.lease {
            Action::setpoint(
                instance,
                contract_port_signature(signature, PortKind::Setpoint),
                request.encode_to_vec(),
                Validity::Lease {
                    valid_for_ms: lease.valid_for_ms(),
                },
            )
            .map_err(|error| crate::anyhow!("leased call operation: {error}"))
        } else {
            Action::command(
                instance,
                contract_port_signature(signature, PortKind::Commands),
                request.encode_to_vec(),
                label,
                Duration::from_secs(5),
                Duration::from_secs(5),
            )
            .map_err(|error| crate::anyhow!("call operation: {error}"))
        }
    }
}

impl<Request, Response> SendOperation for crate::contract::Withdraw<Request, Response> {
    type Response = NoReply;

    fn into_action(self, _label: &str) -> crate::Result<Action> {
        Action::withdraw(
            self.instance(),
            contract_port_signature(self.signature(), PortKind::Setpoint),
        )
        .map_err(|error| crate::anyhow!("withdrawal operation: {error}"))
    }
}

impl<Value> ObservationOperation for crate::contract::Observation<Value>
where
    Value: ScenarioValue,
{
    type Value = Value;

    fn into_capture(self, _name: &str, policy: CapturePolicy) -> crate::Result<super::Capture> {
        let signature = self.signature();
        let port = contract_port_signature(
            signature,
            if signature.retained_latest {
                PortKind::State
            } else {
                PortKind::Sample
            },
        );
        let name = format!("{}/{}", self.instance(), port.name);
        if signature.retained_latest {
            super::Capture::state_with_policy(name, port, policy)
        } else {
            super::Capture::sample_with_policy(name, port, policy)
        }
        .map_err(|error| crate::anyhow!("observation operation: {error}"))
    }
}

fn contract_port_signature(
    signature: crate::contract::MethodSignature,
    kind: PortKind,
) -> PortSignature {
    PortSignature::from_method(signature, kind)
}

/// Value that can be decoded from completed simulation evidence.
pub trait ScenarioValue: Sized {
    fn decode(bytes: &[u8]) -> crate::Result<Self>;
}

/// Marker returned by lease updates and withdrawals, which do not carry a
/// business-response message.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct NoReply;

impl ScenarioValue for NoReply {
    fn decode(bytes: &[u8]) -> crate::Result<Self> {
        if bytes.is_empty() {
            Ok(Self)
        } else {
            Err(crate::anyhow!(
                "operation without a response returned payload bytes"
            ))
        }
    }
}

impl<T> ScenarioValue for T
where
    T: Message + Default,
{
    fn decode(bytes: &[u8]) -> crate::Result<Self> {
        T::decode(bytes)
            .map_err(|error| crate::anyhow!("invalid scenario protobuf evidence: {error}"))
    }
}

/// Typed handle for one staged operation outcome.
#[derive(Debug, PartialEq, Eq)]
pub struct ReplyTicket<T> {
    plan_id: u64,
    index: u32,
    marker: PhantomData<fn() -> T>,
}

impl<T> Copy for ReplyTicket<T> {}
impl<T> Clone for ReplyTicket<T> {
    fn clone(&self) -> Self {
        *self
    }
}

/// Typed handle for one declared observation.
#[derive(Debug, PartialEq, Eq)]
pub struct Capture<T> {
    plan_id: u64,
    name: String,
    policy: Option<CapturePolicy>,
    encoding: CaptureEncoding,
    marker: PhantomData<fn() -> T>,
}

impl<T> Clone for Capture<T> {
    fn clone(&self) -> Self {
        Self {
            plan_id: self.plan_id,
            name: self.name.clone(),
            policy: self.policy,
            encoding: self.encoding,
            marker: PhantomData,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CaptureEncoding {
    Protobuf,
    NativeBodyJson,
}

/// One typed captured value and its retained provenance.
#[derive(Clone, Debug, PartialEq)]
pub struct Observation<T> {
    value: T,
    capture_time: Option<u64>,
    source: Option<String>,
    sequence: Option<u64>,
    gap_before: bool,
    terminal: bool,
}

impl<T> Observation<T> {
    #[must_use]
    pub const fn value(&self) -> &T {
        &self.value
    }

    #[must_use]
    pub const fn capture_time(&self) -> Option<u64> {
        self.capture_time
    }

    #[must_use]
    pub fn source(&self) -> Option<&str> {
        self.source.as_deref()
    }

    /// Original source publication sequence, when runtime provenance is available.
    #[must_use]
    pub const fn sequence(&self) -> Option<u64> {
        self.sequence
    }

    /// Whether records preceding this value were discarded by best-effort retention.
    #[must_use]
    pub const fn gap_before(&self) -> bool {
        self.gap_before
    }

    /// Whether this is the last retained value before the completed final drain.
    #[must_use]
    pub const fn terminal(&self) -> bool {
        self.terminal
    }
}

/// Typed outcome of an operation that reached the finite run boundary.
#[derive(Clone, Debug, PartialEq)]
pub enum ReplyOutcome<T> {
    Received(T),
    Pending,
    ExpiredSimulated,
    ExpiredHost,
    Rejected(String),
}

/// A run whose execution, evidence finalization, and owned cleanup completed.
#[derive(Debug)]
pub struct CompletedRun {
    plan_id: u64,
    run: super::ScenarioRun,
}

impl CompletedRun {
    pub(crate) fn new(plan_id: u64, run: super::ScenarioRun) -> Self {
        Self { plan_id, run }
    }

    /// Inspect the typed terminal outcome for a staged operation.
    pub fn outcome<T>(&self, ticket: ReplyTicket<T>) -> crate::Result<ReplyOutcome<T>>
    where
        T: ScenarioValue,
    {
        self.check_plan(ticket.plan_id)?;
        let label = format!("action-{}", ticket.index);
        let Some(reply) = self.run.command_reply(&label) else {
            return match self.run.outcome(&label) {
                Some(super::StepOutcome::SetpointDelivered { .. })
                | Some(super::StepOutcome::WithdrawAccepted) => {
                    Ok(ReplyOutcome::Received(T::decode(&[])?))
                }
                Some(super::StepOutcome::Rejected { reason })
                | Some(super::StepOutcome::FixtureLost { reason }) => {
                    Ok(ReplyOutcome::Rejected(reason.clone()))
                }
                Some(super::StepOutcome::CommandIssued { .. }) | None => Ok(ReplyOutcome::Pending),
            };
        };
        match reply {
            CommandReply::Accepted { response_bytes } => {
                Ok(ReplyOutcome::Received(T::decode(response_bytes)?))
            }
            CommandReply::ExpiredSimulated => Ok(ReplyOutcome::ExpiredSimulated),
            CommandReply::ExpiredHost => Ok(ReplyOutcome::ExpiredHost),
            CommandReply::Rejected { reason } => Ok(ReplyOutcome::Rejected(reason.clone())),
        }
    }

    /// Require and decode a received response.
    pub fn reply<T>(&self, ticket: ReplyTicket<T>) -> crate::Result<T>
    where
        T: ScenarioValue,
    {
        match self.outcome(ticket)? {
            ReplyOutcome::Received(value) => Ok(value),
            ReplyOutcome::Pending => Err(crate::anyhow!("operation response is pending")),
            ReplyOutcome::ExpiredSimulated => Err(crate::anyhow!(
                "operation expired at the simulated deadline"
            )),
            ReplyOutcome::ExpiredHost => {
                Err(crate::anyhow!("operation expired at the host deadline"))
            }
            ReplyOutcome::Rejected(reason) => {
                Err(crate::anyhow!("operation was rejected: {reason}"))
            }
        }
    }

    /// Return the latest retained observation.
    pub fn latest<T>(&self, capture: &Capture<T>) -> crate::Result<Observation<T>>
    where
        T: ScenarioValue,
    {
        self.history(capture)?
            .pop()
            .ok_or_else(|| crate::anyhow!("capture has no published value"))
    }

    /// Return every retained value in capture order.
    pub fn history<T>(&self, capture: &Capture<T>) -> crate::Result<Vec<Observation<T>>>
    where
        T: ScenarioValue,
    {
        self.check_plan(capture.plan_id)?;
        let name = capture.name.clone();
        let record = self
            .run
            .capture(&name)
            .ok_or_else(|| crate::anyhow!("capture `{name}` is missing from completed evidence"))?;
        if capture.encoding == CaptureEncoding::NativeBodyJson {
            return Err(crate::anyhow!(
                "native body captures require `body_history` until the generated observation contract is available"
            ));
        }
        if let CaptureRecord::Observations {
            records,
            gap_before_first,
            terminal,
            ..
        } = record
        {
            let last = records.len().saturating_sub(1);
            return records
                .iter()
                .enumerate()
                .map(|(index, record)| {
                    Ok(Observation {
                        value: T::decode(&record.payload)?,
                        capture_time: Some(record.capture_time_ns),
                        source: Some(record.source.clone()),
                        sequence: Some(record.sequence),
                        gap_before: index == 0 && *gap_before_first,
                        terminal: *terminal && index == last,
                    })
                })
                .collect();
        }
        let payloads: Vec<&[u8]> = match record {
            CaptureRecord::State(bytes) => vec![bytes.as_slice()],
            CaptureRecord::Samples(values) | CaptureRecord::Events(values) => {
                values.iter().map(Vec::as_slice).collect()
            }
            CaptureRecord::NativeBody(_) => {
                return Err(crate::anyhow!("capture `{name}` is native-body evidence"));
            }
            CaptureRecord::Observations { .. } => unreachable!("handled above"),
        };
        let payload_count = payloads.len();
        payloads
            .into_iter()
            .enumerate()
            .map(|(index, bytes)| {
                Ok(Observation {
                    value: T::decode(bytes)?,
                    capture_time: None,
                    source: Some(name.clone()),
                    sequence: u64::try_from(index).ok(),
                    gap_before: false,
                    terminal: index + 1 == payload_count,
                })
            })
            .collect()
    }

    /// Decode the retained native-body history.
    pub fn body_history(
        &self,
        capture: &Capture<NativeBodySample>,
    ) -> crate::Result<Vec<Observation<NativeBodySample>>> {
        self.check_plan(capture.plan_id)?;
        let name = capture.name.clone();
        let CaptureRecord::NativeBody(bytes) = self
            .run
            .capture(&name)
            .ok_or_else(|| crate::anyhow!("native body capture `{name}` is missing"))?
        else {
            return Err(crate::anyhow!(
                "capture `{name}` is not native-body evidence"
            ));
        };
        let samples: Vec<NativeBodySample> = serde_json::from_slice(bytes)
            .map_err(|error| crate::anyhow!("invalid native body evidence: {error}"))?;
        let last = samples.len().saturating_sub(1);
        Ok(samples
            .into_iter()
            .enumerate()
            .map(|(index, sample)| Observation {
                capture_time: Some(sample.boundary),
                source: Some(name.clone()),
                sequence: Some(sample.boundary),
                gap_before: false,
                terminal: index == last,
                value: sample,
            })
            .collect())
    }

    fn check_plan(&self, plan_id: u64) -> crate::Result<()> {
        if self.plan_id == plan_id {
            Ok(())
        } else {
            Err(crate::anyhow!(
                "handle belongs to a different scenario plan or run"
            ))
        }
    }
}

fn capture_name(capture: &super::Capture) -> &str {
    match capture {
        super::Capture::State { name, .. }
        | super::Capture::Sample { name, .. }
        | super::Capture::Event { name, .. }
        | super::Capture::NativeBody { name, .. } => name,
    }
}

#[cfg(test)]
mod tests {
    use std::collections::BTreeMap;

    use super::*;
    use crate::scenario::plan_support::{CapturedObservation, Quantum, ScenarioRun};

    #[derive(Clone, PartialEq, Message)]
    struct TestValue {
        #[prost(uint32, tag = "1")]
        value: u32,
    }

    fn signature() -> PortSignature {
        PortSignature::new(
            "motion/manual",
            "phoxal.motion.v1.Motion",
            "Manual",
            PortKind::Setpoint,
            "phoxal.motion.v1.MotionIntent",
            "phoxal.Empty",
        )
    }

    fn leased_step(label: &str, boundary: u32, byte: u8, valid_for_ms: u64) -> Step {
        Step::new(
            label,
            boundary,
            Action::setpoint(
                "motion",
                signature(),
                vec![byte],
                Validity::Lease { valid_for_ms },
            )
            .expect("leased action"),
        )
    }

    fn withdraw_step(label: &str, boundary: u32) -> Step {
        Step::new(
            label,
            boundary,
            Action::withdraw("motion", signature()).expect("withdrawal"),
        )
    }

    fn boundaries(steps: &[Step], prefix: &str) -> Vec<u32> {
        steps
            .iter()
            .filter(|step| step.label.starts_with(prefix))
            .map(|step| step.boundary)
            .collect()
    }

    #[test]
    fn lease_expansion_is_finite_and_stops_before_run_end() {
        let steps = expand_lease_renewals(
            vec![leased_step("action-0", 0, 1, 8)],
            10,
            Quantum::from_micros(2_000).expect("quantum"),
        )
        .expect("lease expansion");

        assert_eq!(boundaries(&steps, "action-0"), vec![0, 2, 4, 6, 8]);
        assert!(steps.iter().all(|step| step.boundary < 10));
    }

    #[test]
    fn replacement_cancels_old_renewals_and_starts_its_own() {
        let steps = expand_lease_renewals(
            vec![
                leased_step("action-0", 0, 1, 8),
                leased_step("action-1", 5, 2, 8),
            ],
            12,
            Quantum::from_micros(2_000).expect("quantum"),
        )
        .expect("lease expansion");

        assert_eq!(boundaries(&steps, "action-0"), vec![0, 2, 4]);
        assert_eq!(boundaries(&steps, "action-1"), vec![5, 7, 9, 11]);
    }

    #[test]
    fn withdrawal_cancels_future_renewals() {
        let steps = expand_lease_renewals(
            vec![
                leased_step("action-0", 0, 1, 8),
                withdraw_step("action-1", 5),
            ],
            12,
            Quantum::from_micros(2_000).expect("quantum"),
        )
        .expect("lease expansion");

        assert_eq!(boundaries(&steps, "action-0"), vec![0, 2, 4]);
        assert_eq!(boundaries(&steps, "action-1"), vec![5]);
    }

    #[test]
    fn lease_shorter_than_native_quantum_is_rejected() {
        let error = expand_lease_renewals(
            vec![leased_step("action-0", 0, 1, 1)],
            2,
            Quantum::from_micros(2_000).expect("quantum"),
        )
        .expect_err("short lease must fail");

        assert!(
            error
                .to_string()
                .contains("shorter than one native quantum")
        );
    }

    #[test]
    fn completed_run_preserves_observation_provenance_gap_and_terminal_evidence() {
        let record = CapturedObservation {
            payload: TestValue { value: 7 }.encode_to_vec(),
            source: "world.pose@epoch-2".to_owned(),
            capture_time_ns: 42,
            sequence: 9,
        };
        let run = ScenarioRun::from_sealed(
            Vec::new(),
            BTreeMap::from([(
                "world/pose".to_owned(),
                CaptureRecord::Observations {
                    kind: "sample".to_owned(),
                    records: vec![record],
                    gap_before_first: true,
                    complete: false,
                    terminal: true,
                },
            )]),
            BTreeMap::new(),
            None,
            true,
        );
        let completed = CompletedRun::new(42, run);
        let capture = Capture::<TestValue> {
            plan_id: 42,
            name: "world/pose".to_owned(),
            policy: Some(CapturePolicy::BestEffortHistory { capacity: 1 }),
            encoding: CaptureEncoding::Protobuf,
            marker: PhantomData,
        };

        let history = completed.history(&capture).expect("typed history");
        assert_eq!(history[0].value().value, 7);
        assert_eq!(history[0].source(), Some("world.pose@epoch-2"));
        assert_eq!(history[0].capture_time(), Some(42));
        assert_eq!(history[0].sequence(), Some(9));
        assert!(history[0].gap_before());
        assert!(history[0].terminal());
    }
}
