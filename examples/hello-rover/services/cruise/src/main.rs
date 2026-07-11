//! `hello-rover-cruise` - the example user service for the `hello-rover`
//! project (`framework/examples/hello-rover`).
//!
//! It commands a constant forward speed while the official `safety`
//! participant's authorization allows it, and holds still otherwise. This is
//! the minimal shape of a hand-authored user participant: one
//! `#[derive(phoxal::Config)]` block read from `robot.yaml`'s
//! `services.cruise.config`, one `#[derive(phoxal::Api)]` bus handle struct,
//! and a `#[phoxal::service]` + `#[phoxal::behavior]` pair wiring them
//! together. It subscribes the official `safety/authorization` contract and
//! publishes the official `drive/target` contract, both from
//! `phoxal_api::y2026_1` - a user service authors against official contracts,
//! it never mints its own.
//!
//! See `framework/docs/GETTING_STARTED.md` for the full walkthrough this
//! service is built for.

use anyhow::Result;
use phoxal::prelude::*;
use phoxal_api::y2026_1 as api;

#[derive(serde::Deserialize, phoxal::Config)]
struct Config {
    /// Constant forward speed to command while safety authorizes driving.
    cruise_speed_mps: f32,
}

#[derive(phoxal::Api)]
struct Api {
    authorization: Latest<api::safety::SafetyAuthorization>,
    target: Publisher<api::drive::Target>,
}

#[phoxal::service(id = "cruise")]
struct Cruise {
    cruise_speed_mps: f32,
}

#[phoxal::behavior]
impl Cruise {
    #[setup]
    async fn setup(
        ctx: &mut SetupContext<Self>,
        config: Self::Config,
    ) -> Result<(Self, Self::Api)> {
        Ok((
            Self {
                cruise_speed_mps: config.cruise_speed_mps,
            },
            Self::Api {
                authorization: ctx
                    .latest(api::topic::new().safety().authorization())
                    .await?,
                target: ctx.publisher(api::topic::new().drive().target()).await?,
            },
        ))
    }

    #[step(hz = 10)]
    async fn step(&mut self, api: &mut Self::Api, step: StepContext) -> Result<()> {
        let linear_x_mps = commanded_speed(self.cruise_speed_mps, api.authorization.latest());

        api.target
            .publish_at(
                step.time(),
                api::drive::Target {
                    linear_x_mps,
                    angular_z_radps: 0.0,
                    curvature_limit_radpm: None,
                },
            )
            .await?;
        Ok(())
    }
}

/// The speed to command this step: the configured cruise speed, clamped to
/// the safety participant's approved forward-velocity bound, or zero
/// (fail-safe) while no authorization has arrived yet or the decision is not
/// `Allow`.
fn commanded_speed(
    cruise_speed_mps: f32,
    authorization: Option<api::safety::SafetyAuthorization>,
) -> f32 {
    let Some(authorization) = authorization else {
        return 0.0;
    };
    if authorization.decision != api::safety::SafetyDecision::Allow {
        return 0.0;
    }
    let max_linear_x_mps = authorization.approved_motion.linear_x_mps.max as f32;
    cruise_speed_mps.min(max_linear_x_mps).max(0.0)
}

fn main() -> phoxal::Result<()> {
    phoxal::run::<Cruise>()
}

#[cfg(test)]
mod tests {
    use super::{Cruise, commanded_speed};
    use phoxal::participant::{ContractRole, Participant, ParticipantApi};
    use phoxal_api::ContractBody;
    use phoxal_api::y2026_1 as api;

    #[test]
    fn no_authorization_yet_holds_still() {
        assert_eq!(commanded_speed(0.5, None), 0.0);
    }

    #[test]
    fn non_allow_decision_holds_still() {
        let authorization = authorization(api::safety::SafetyDecision::Stop, 1.0);
        assert_eq!(commanded_speed(0.5, Some(authorization)), 0.0);
    }

    #[test]
    fn allow_decision_clamps_to_approved_bound() {
        let authorization = authorization(api::safety::SafetyDecision::Allow, 0.3);
        assert_eq!(commanded_speed(0.5, Some(authorization)), 0.3);
    }

    #[test]
    fn allow_decision_under_bound_uses_cruise_speed() {
        let authorization = authorization(api::safety::SafetyDecision::Allow, 1.0);
        assert_eq!(commanded_speed(0.2, Some(authorization)), 0.2);
    }

    #[test]
    fn api_declares_the_expected_contracts() {
        assert_eq!(<Cruise as Participant>::ID, "cruise");

        let contracts = <<Cruise as Participant>::Api as ParticipantApi>::CONTRACTS;
        assert!(contracts.iter().any(|c| {
            c.topic == <api::safety::SafetyAuthorization as ContractBody>::TOPIC
                && c.role == ContractRole::Subscribe
        }));
        assert!(contracts.iter().any(|c| {
            c.topic == <api::drive::Target as ContractBody>::TOPIC
                && c.role == ContractRole::Publish
        }));
    }

    fn authorization(
        decision: api::safety::SafetyDecision,
        max_linear_x_mps: f64,
    ) -> api::safety::SafetyAuthorization {
        api::safety::SafetyAuthorization {
            decision,
            approved_motion: api::safety::MotionConstraint {
                linear_x_mps: api::safety::Constraint {
                    min: 0.0,
                    max: max_linear_x_mps,
                },
                angular_z_radps: api::safety::Constraint { min: 0.0, max: 0.0 },
            },
            reasons: Vec::new(),
            source_revision: api::safety::SafetySourceRevision {
                localization: None,
                map: None,
            },
            expires_at_ns: None,
        }
    }
}
