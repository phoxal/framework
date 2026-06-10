use std::collections::BTreeMap;
use std::time::Duration;

use crate::core::{EmergencyStopInputs, EvaluationOutcome, RangeSafetyClass};
use anyhow::Result;
use phoxal::api::component::v1::capability::{emergency_stop as component_emergency_stop, range};
use phoxal::api::localize::v1::LocalizationState;
use phoxal::api::map::v1::TraversabilitySummary;
use phoxal::api::safety::v1::{
    EmergencyStopRequest, SafetyAuthorization, SafetyDecision, SafetyReasonCode,
    SafetySourceRevision, State,
};
use phoxal::api::v1::topic;
use phoxal::bus::typed::Received;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::v1::Robot;
use phoxal::runtime::clock::Step;
use phoxal::runtime::decision_log::DecisionLog;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};
use phoxal::runtime::{Io, Runtime, RuntimeInputs, TopicPublisher};

use crate::range_classification::{classify_safety_range_inputs, range_source_id};
use crate::selector::{detect_safety_emergency_stop_inputs, detect_safety_range_inputs};

const CLOCK_PERIOD: Duration = Duration::from_millis(50);
const SAFETY_AUTHORIZATION_VALIDITY_NS: u64 = 200_000_000; // 200 ms
const OPERATOR_EMERGENCY_STOP_REQUEST_TIMEOUT_NS: u64 = 500_000_000; // 500 ms

#[derive(Clone)]
pub struct Config {
    range_inputs: Vec<CapabilityRef>,
    emergency_stop_inputs: Vec<CapabilityRef>,
    range_classes: BTreeMap<String, RangeSafetyClass>,
    clock_period: Duration,
}

impl Config {
    pub fn from_robot(robot: &Robot) -> Result<Self> {
        Ok(Self {
            range_inputs: detect_safety_range_inputs(robot),
            emergency_stop_inputs: detect_safety_emergency_stop_inputs(robot),
            range_classes: classify_safety_range_inputs(robot),
            clock_period: CLOCK_PERIOD,
        })
    }

    pub const fn clock_period(&self) -> Duration {
        self.clock_period
    }
}

pub enum Input {
    Range {
        source_id: String,
        sample: Received<range::Sample>,
    },
    EmergencyStop {
        source_id: String,
        state: Received<component_emergency_stop::State>,
    },
    OperatorEmergencyStopRequest(Received<EmergencyStopRequest>),
    LocalizationState(Box<Received<LocalizationState>>),
    MapTraversabilitySummary(Received<TraversabilitySummary>),
}

pub struct SafetyRuntime {
    latest_range: BTreeMap<String, Received<range::Sample>>,
    latest_emergency_stop: BTreeMap<String, Received<component_emergency_stop::State>>,
    latest_operator_emergency_stop_request: Option<Received<EmergencyStopRequest>>,
    latest_localize_state: Option<Received<LocalizationState>>,
    latest_traversability_summary: Option<Received<TraversabilitySummary>>,
    range_classes: BTreeMap<String, RangeSafetyClass>,
    decision_log: DecisionLog<SafetyLogKey>,
    authorization_publisher: TopicPublisher<SafetyAuthorization>,
    state_publisher: TopicPublisher<State>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
struct SafetyLogKey {
    decision: SafetyDecision,
    reason_codes: Vec<SafetyReasonCode>,
}

#[async_trait::async_trait]
impl Runtime for SafetyRuntime {
    const RUNTIME_ID: &'static str = "safety";

    type Args = EmptyArgs;
    type Config = Config;
    type Input = Input;

    fn config(_args: &Self::Args, common: &RobotRuntimeArgs) -> Result<Self::Config> {
        Config::from_robot(&common.robot()?)
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        for capability in &config.range_inputs {
            let source_id = range_source_id(capability);
            io.subscribe_topic(
                topic::new()
                    .v1()
                    .component(capability.component_id.clone())
                    .range(capability.capability_id.clone())
                    .data(),
                {
                    let source_id = source_id.clone();
                    move |sample| Input::Range {
                        source_id: source_id.clone(),
                        sample,
                    }
                },
            )
            .await?;
        }

        for capability in &config.emergency_stop_inputs {
            let source_id = capability.to_string();
            io.subscribe_topic(
                topic::new()
                    .v1()
                    .component(capability.component_id.clone())
                    .emergency_stop(capability.capability_id.clone())
                    .data(),
                {
                    let source_id = source_id.clone();
                    move |state| Input::EmergencyStop {
                        source_id: source_id.clone(),
                        state,
                    }
                },
            )
            .await?;
        }

        io.subscribe_topic(
            topic::new().v1().safety().emergency_stop_request(),
            Input::OperatorEmergencyStopRequest,
        )
        .await?;

        io.subscribe_topic(topic::new().v1().localize().state(), |sample| {
            Input::LocalizationState(Box::new(sample))
        })
        .await?;
        io.subscribe_topic(
            topic::new().v1().map().traversability_summary(),
            Input::MapTraversabilitySummary,
        )
        .await?;

        let authorization_publisher = io
            .publisher_topic(topic::new().v1().safety().authorization())
            .await?;
        let state_topic = topic::new().v1().safety().state();
        let state_publisher = io.publisher_topic(state_topic.clone()).await?;

        Ok(Self {
            latest_range: BTreeMap::new(),
            latest_emergency_stop: BTreeMap::new(),
            latest_operator_emergency_stop_request: None,
            latest_localize_state: None,
            latest_traversability_summary: None,
            range_classes: config.range_classes,
            decision_log: DecisionLog::from_topic(Self::RUNTIME_ID, &state_topic),
            authorization_publisher,
            state_publisher,
        })
    }

    async fn step(&mut self, step: Step, inputs: RuntimeInputs<Self::Input>) -> Result<()> {
        for input in inputs {
            match input {
                Input::Range { source_id, sample } => {
                    self.latest_range.insert(source_id, sample);
                }
                Input::EmergencyStop { source_id, state } => {
                    self.latest_emergency_stop.insert(source_id, state);
                }
                Input::OperatorEmergencyStopRequest(request) => {
                    self.latest_operator_emergency_stop_request = Some(request);
                }
                Input::LocalizationState(sample) => {
                    self.latest_localize_state = Some(*sample);
                }
                Input::MapTraversabilitySummary(sample) => {
                    self.latest_traversability_summary = Some(sample);
                }
            }
        }

        let now_ns = step.tick.time_ns();
        let outcome = EvaluationOutcome::evaluate(
            &self.latest_range,
            &self.range_classes,
            self.latest_localize_state.as_ref(),
            EmergencyStopInputs {
                hardware_engaged: hardware_emergency_stop_engaged(&self.latest_emergency_stop),
                operator_engaged: operator_emergency_stop_engaged(
                    self.latest_operator_emergency_stop_request.as_ref(),
                    now_ns,
                ),
            },
            now_ns,
        );

        let authorization = SafetyAuthorization {
            decision: outcome.decision,
            source_revision: SafetySourceRevision {
                localization: self
                    .latest_localize_state
                    .as_ref()
                    .and_then(|state| state.value.revision),
                map: self
                    .latest_traversability_summary
                    .as_ref()
                    .map(|summary| summary.value.map_revision),
                raw_sources: Vec::new(),
            },
            approved_motion: outcome.motion_constraint,
            reasons: outcome.reasons.clone(),
            expires_at_ns: authorization_expires_at_ns(now_ns),
        };
        self.authorization_publisher
            .put(now_ns, &authorization)
            .await?;

        let state = State {
            decision: outcome.decision,
            active_reasons: outcome.reasons,
        };
        self.decision_log.observe(
            now_ns,
            SafetyLogKey {
                decision: state.decision,
                reason_codes: state
                    .active_reasons
                    .iter()
                    .map(|reason| reason.code)
                    .collect(),
            },
        );
        self.state_publisher.put(now_ns, &state).await?;
        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::ScenarioDescriptor] {
        crate::scenarios::SCENARIOS
    }

    async fn run_scenario(name: &str, common: &RobotRuntimeArgs, _args: &Self::Args) -> Result<()> {
        crate::scenarios::run(name, common).await
    }
}

const fn authorization_expires_at_ns(now_ns: u64) -> Option<u64> {
    Some(now_ns + SAFETY_AUTHORIZATION_VALIDITY_NS)
}

fn hardware_emergency_stop_engaged(
    states: &BTreeMap<String, Received<component_emergency_stop::State>>,
) -> bool {
    states.values().any(|state| state.value.engaged)
}

fn operator_emergency_stop_engaged(
    request: Option<&Received<EmergencyStopRequest>>,
    now_ns: u64,
) -> bool {
    request.is_some_and(|request| {
        request.value.engaged
            && request.at_ns.is_some_and(|at_ns| {
                now_ns.saturating_sub(at_ns) <= OPERATOR_EMERGENCY_STOP_REQUEST_TIMEOUT_NS
            })
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    const NOW_NS: u64 = 2_000_000_000;

    #[test]
    fn authorization_carries_expiry() {
        assert_eq!(
            authorization_expires_at_ns(NOW_NS),
            Some(NOW_NS + SAFETY_AUTHORIZATION_VALIDITY_NS)
        );
    }

    #[test]
    fn hardware_emergency_stop_engaged_if_any_input_is_engaged() {
        let states = BTreeMap::from([
            (
                "left.e_stop".to_string(),
                received(NOW_NS, component_emergency_stop::State { engaged: false }),
            ),
            (
                "right.e_stop".to_string(),
                received(NOW_NS, component_emergency_stop::State { engaged: true }),
            ),
        ]);

        assert!(hardware_emergency_stop_engaged(&states));
    }

    #[test]
    fn stale_operator_emergency_stop_request_is_not_engaged() {
        let request = received(
            NOW_NS - OPERATOR_EMERGENCY_STOP_REQUEST_TIMEOUT_NS - 1,
            EmergencyStopRequest { engaged: true },
        );

        assert!(!operator_emergency_stop_engaged(Some(&request), NOW_NS));
    }

    fn received<T>(timestamp_ns: u64, value: T) -> Received<T> {
        Received {
            at_ns: Some(timestamp_ns),
            value,
        }
    }
}
