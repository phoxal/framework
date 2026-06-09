use std::collections::BTreeMap;
use std::time::Duration;

use crate::core::{EmergencyStopInputs, EvaluationOutcome, RangeSafetyClass};
use anyhow::Result;
use phoxal::api::component::v1::capability::{emergency_stop as component_emergency_stop, range};
use phoxal::api::localize::v1::{LocalizationState, state as localize_state};
use phoxal::api::safety::v1::{
    EmergencyStopRequest, SafetyAuthorization, SafetyDecision, SafetyReasonCode,
    SafetySourceRevision, State, authorization as safety_authorization,
    emergency_stop_request as safety_emergency_stop_request, state as safety_state,
};
use phoxal::bus::pubsub::Stamped;
use phoxal::bus::zenoh::TypedSchema;
use phoxal::model::component::v1::CapabilityRef;
use phoxal::model::structure::Structure;
use phoxal::runtime::clock::Step;
use phoxal::runtime::decision_log::DecisionLog;
use phoxal::runtime::runtime::{Io, Publisher, Runtime, RuntimeInputs};
use phoxal::runtime::staged::Robot;
use phoxal::runtime::{EmptyArgs, RobotRuntimeArgs};

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
    pub fn from_robot(robot: &Robot, structure: &Structure) -> Result<Self> {
        Ok(Self {
            range_inputs: detect_safety_range_inputs(robot),
            emergency_stop_inputs: detect_safety_emergency_stop_inputs(robot),
            range_classes: classify_safety_range_inputs(robot, structure),
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
        sample: Stamped<range::Sample>,
    },
    EmergencyStop {
        source_id: String,
        state: Stamped<component_emergency_stop::State>,
    },
    OperatorEmergencyStopRequest(Stamped<EmergencyStopRequest>),
    LocalizationState(Box<Stamped<LocalizationState>>),
}

pub struct SafetyRuntime {
    latest_range: BTreeMap<String, Stamped<range::Sample>>,
    latest_emergency_stop: BTreeMap<String, Stamped<component_emergency_stop::State>>,
    latest_operator_emergency_stop_request: Option<Stamped<EmergencyStopRequest>>,
    latest_localize_state: Option<Stamped<LocalizationState>>,
    range_classes: BTreeMap<String, RangeSafetyClass>,
    decision_log: DecisionLog<SafetyLogKey>,
    authorization_publisher: Publisher<Stamped<SafetyAuthorization>>,
    state_publisher: Publisher<Stamped<State>>,
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
        Config::from_robot(&common.robot()?, &common.structure()?)
    }

    fn clock_period(config: &Self::Config) -> Duration {
        config.clock_period()
    }

    async fn new(io: &mut Io<Self::Input>, config: Self::Config) -> Result<Self> {
        for capability in &config.range_inputs {
            let source_id = range_source_id(capability);
            let topic = range::topic(&capability.component_id, &capability.capability_id);
            io.subscribe::<Stamped<range::Sample>, _>(&topic, {
                let source_id = source_id.clone();
                move |sample| Input::Range {
                    source_id: source_id.clone(),
                    sample,
                }
            })
            .await?;
        }

        for capability in &config.emergency_stop_inputs {
            let source_id = capability.to_string();
            let topic = component_emergency_stop::topic(
                &capability.component_id,
                &capability.capability_id,
            );
            io.subscribe::<Stamped<component_emergency_stop::State>, _>(&topic, {
                let source_id = source_id.clone();
                move |state| Input::EmergencyStop {
                    source_id: source_id.clone(),
                    state,
                }
            })
            .await?;
        }

        io.subscribe::<Stamped<EmergencyStopRequest>, _>(
            safety_emergency_stop_request::TOPIC,
            Input::OperatorEmergencyStopRequest,
        )
        .await?;

        io.subscribe::<Stamped<LocalizationState>, _>(localize_state::TOPIC, |sample| {
            Input::LocalizationState(Box::new(sample))
        })
        .await?;

        let authorization_publisher = io
            .publisher::<Stamped<SafetyAuthorization>>(safety_authorization::TOPIC)
            .await?;
        let state_publisher = io.publisher::<Stamped<State>>(safety_state::TOPIC).await?;

        Ok(Self {
            latest_range: BTreeMap::new(),
            latest_emergency_stop: BTreeMap::new(),
            latest_operator_emergency_stop_request: None,
            latest_localize_state: None,
            range_classes: config.range_classes,
            decision_log: DecisionLog::new(
                Self::RUNTIME_ID,
                safety_state::TOPIC,
                <State as TypedSchema>::SCHEMA_NAME,
                <State as TypedSchema>::SCHEMA_VERSION,
            ),
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
                    .and_then(|state| state.data.revision),
                map: None,
                raw_sources: Vec::new(),
            },
            approved_motion: outcome.motion_constraint,
            reasons: outcome.reasons.clone(),
            expires_at_ns: authorization_expires_at_ns(now_ns),
        };
        self.authorization_publisher
            .put(&Stamped::new(now_ns, authorization))
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
        self.state_publisher
            .put(&Stamped::new(now_ns, state))
            .await?;
        Ok(())
    }

    fn scenarios() -> &'static [phoxal::runtime::runtime::ScenarioDescriptor] {
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
    states: &BTreeMap<String, Stamped<component_emergency_stop::State>>,
) -> bool {
    states.values().any(|state| state.data.engaged)
}

fn operator_emergency_stop_engaged(
    request: Option<&Stamped<EmergencyStopRequest>>,
    now_ns: u64,
) -> bool {
    request.is_some_and(|request| {
        request.data.engaged
            && now_ns.saturating_sub(request.timestamp_ns)
                <= OPERATOR_EMERGENCY_STOP_REQUEST_TIMEOUT_NS
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
                Stamped::new(NOW_NS, component_emergency_stop::State { engaged: false }),
            ),
            (
                "right.e_stop".to_string(),
                Stamped::new(NOW_NS, component_emergency_stop::State { engaged: true }),
            ),
        ]);

        assert!(hardware_emergency_stop_engaged(&states));
    }

    #[test]
    fn stale_operator_emergency_stop_request_is_not_engaged() {
        let request = Stamped::new(
            NOW_NS - OPERATOR_EMERGENCY_STOP_REQUEST_TIMEOUT_NS - 1,
            EmergencyStopRequest { engaged: true },
        );

        assert!(!operator_emergency_stop_engaged(Some(&request), NOW_NS));
    }
}
